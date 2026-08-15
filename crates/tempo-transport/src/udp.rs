//! UDP transport.
//!
//! The baseline for dedicated servers: lowest latency, no handshake of its own, no ordering or
//! delivery guarantees. Everything that makes a UDP endpoint safe to expose — encrypted connect
//! tokens, challenge/response against address spoofing, replay windows, rate limiting — lives in
//! the connection layer above, per [ADR-0015](../../../docs/adr/0015-connect-tokens-and-security.md).
//! This module is the socket and nothing more.
//!
//! Peers are addressed by [`PeerId`], not by socket address, so the layers above are identical
//! across every transport. The mapping between the two is maintained here.

use std::collections::HashMap;
use std::io::ErrorKind;
use std::net::{SocketAddr, ToSocketAddrs, UdpSocket};

use crate::{PeerId, Received, Timestamp, Transport, TransportError, MTU};

/// A UDP endpoint.
#[derive(Debug)]
pub struct UdpTransport {
    socket: UdpSocket,
    local: PeerId,
    /// Where to send for a given peer.
    addr_of: HashMap<PeerId, SocketAddr>,
    /// Who a datagram came from.
    peer_of: HashMap<SocketAddr, PeerId>,
    /// Next identifier handed to an unrecognised sender.
    next_auto_id: u64,
    /// Whether unrecognised senders are admitted at all.
    accept_unknown: bool,
    recv_buf: Vec<u8>,
}

impl UdpTransport {
    /// Binds a socket and returns a non-blocking endpoint.
    ///
    /// Non-blocking is not optional: the tick loop polls the socket and must never stall on it. A
    /// blocking read here would couple frame time to network arrival.
    pub fn bind(addr: impl ToSocketAddrs, local: PeerId) -> Result<UdpTransport, TransportError> {
        let socket = UdpSocket::bind(addr).map_err(io)?;
        socket.set_nonblocking(true).map_err(io)?;
        Ok(UdpTransport {
            socket,
            local,
            addr_of: HashMap::new(),
            peer_of: HashMap::new(),
            next_auto_id: 1 << 32,
            accept_unknown: false,
            recv_buf: vec![0u8; MTU],
        })
    }

    /// The address actually bound, which resolves port 0 to the one the OS chose.
    pub fn local_addr(&self) -> Result<SocketAddr, TransportError> {
        self.socket.local_addr().map_err(io)
    }

    /// Associates a peer with an address.
    pub fn add_peer(&mut self, peer: PeerId, addr: SocketAddr) {
        self.addr_of.insert(peer, addr);
        self.peer_of.insert(addr, peer);
    }

    /// Forgets a peer.
    pub fn remove_peer(&mut self, peer: PeerId) {
        if let Some(addr) = self.addr_of.remove(&peer) {
            self.peer_of.remove(&addr);
        }
    }

    /// The address registered for a peer.
    pub fn peer_addr(&self, peer: PeerId) -> Option<SocketAddr> {
        self.addr_of.get(&peer).copied()
    }

    /// Whether datagrams from unregistered addresses are admitted.
    ///
    /// Off by default, and that default matters: a server that admits any sender allocates state
    /// for spoofed source addresses, which is the resource-exhaustion vector the challenge/response
    /// in [ADR-0015](../../../docs/adr/0015-connect-tokens-and-security.md) exists to close. Turn it
    /// on only where the connection layer performs that handshake, or in tests.
    pub fn set_accept_unknown(&mut self, accept: bool) {
        self.accept_unknown = accept;
    }

    fn peer_for(&mut self, addr: SocketAddr) -> Option<PeerId> {
        if let Some(&p) = self.peer_of.get(&addr) {
            return Some(p);
        }
        if !self.accept_unknown {
            return None;
        }
        let p = PeerId(self.next_auto_id);
        self.next_auto_id += 1;
        self.add_peer(p, addr);
        Some(p)
    }
}

impl Transport for UdpTransport {
    fn send(&mut self, to: PeerId, data: &[u8], _now: Timestamp) -> Result<(), TransportError> {
        if data.len() > MTU {
            return Err(TransportError::PayloadTooLarge {
                len: data.len(),
                mtu: MTU,
            });
        }
        let addr = self
            .addr_of
            .get(&to)
            .copied()
            .ok_or(TransportError::UnknownPeer(to))?;
        match self.socket.send_to(data, addr) {
            Ok(_) => Ok(()),
            // A full send buffer is congestion, not failure. Dropping is the correct response on an
            // unreliable transport — the layer above already handles loss.
            Err(e) if e.kind() == ErrorKind::WouldBlock => Ok(()),
            Err(e) => Err(io(e)),
        }
    }

    fn recv(&mut self, _now: Timestamp) -> Option<Received> {
        loop {
            // Move the buffer out so the socket read does not hold a borrow of `self`, which
            // `peer_for` needs mutably.
            let mut buf = std::mem::take(&mut self.recv_buf);
            let result = self.socket.recv_from(&mut buf);

            match result {
                Ok((len, addr)) => {
                    let data = buf[..len].to_vec();
                    self.recv_buf = buf;
                    match self.peer_for(addr) {
                        Some(from) => return Some(Received { from, data }),
                        // An unregistered sender is dropped and the loop tries the next datagram.
                        // Deliberately silent: logging here would let anyone fill the logs by
                        // sending to the port.
                        None => continue,
                    }
                }
                // WouldBlock means nothing is queued; any other error is treated the same way,
                // because a transient socket error must not stall the tick loop.
                Err(_) => {
                    self.recv_buf = buf;
                    return None;
                }
            }
        }
    }

    fn local_peer(&self) -> PeerId {
        self.local
    }
}

fn io(e: std::io::Error) -> TransportError {
    TransportError::Io(e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::drain;

    fn bound(id: u64) -> UdpTransport {
        UdpTransport::bind("127.0.0.1:0", PeerId(id)).expect("bind loopback")
    }

    /// UDP delivery is asynchronous even on loopback; poll briefly rather than assuming immediacy.
    fn recv_within(t: &mut UdpTransport, tries: u32) -> Vec<Received> {
        let mut out = Vec::new();
        for _ in 0..tries {
            out.extend(drain(t, Timestamp::ZERO));
            if !out.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        out
    }

    #[test]
    fn datagrams_round_trip_over_loopback() {
        let mut a = bound(0);
        let mut b = bound(1);
        let (aa, ba) = (a.local_addr().unwrap(), b.local_addr().unwrap());
        a.add_peer(PeerId(1), ba);
        b.add_peer(PeerId(0), aa);

        a.send(PeerId(1), b"ping", Timestamp::ZERO).unwrap();
        let got = recv_within(&mut b, 50);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].from, PeerId(0));
        assert_eq!(got[0].data, b"ping");

        b.send(PeerId(0), b"pong", Timestamp::ZERO).unwrap();
        let got = recv_within(&mut a, 50);
        assert_eq!(got[0].data, b"pong");
    }

    #[test]
    fn sending_to_an_unregistered_peer_is_an_error() {
        let mut a = bound(0);
        assert!(matches!(
            a.send(PeerId(99), b"x", Timestamp::ZERO),
            Err(TransportError::UnknownPeer(PeerId(99)))
        ));
    }

    #[test]
    fn unknown_senders_are_ignored_by_default() {
        // The default that matters: admitting any sender means allocating state for spoofed source
        // addresses.
        let mut server = bound(0);
        let mut stranger = bound(1);
        stranger.add_peer(PeerId(0), server.local_addr().unwrap());
        stranger
            .send(PeerId(0), b"unsolicited", Timestamp::ZERO)
            .unwrap();

        std::thread::sleep(std::time::Duration::from_millis(20));
        assert!(drain(&mut server, Timestamp::ZERO).is_empty());
    }

    #[test]
    fn unknown_senders_can_be_admitted_explicitly() {
        let mut server = bound(0);
        server.set_accept_unknown(true);
        let mut client = bound(1);
        client.add_peer(PeerId(0), server.local_addr().unwrap());
        client.send(PeerId(0), b"hello", Timestamp::ZERO).unwrap();

        let got = recv_within(&mut server, 50);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].data, b"hello");
        // The sender was assigned an identifier and is now addressable.
        assert!(server.peer_addr(got[0].from).is_some());
    }

    #[test]
    fn oversized_datagrams_are_refused() {
        let mut a = bound(0);
        a.add_peer(PeerId(1), "127.0.0.1:9".parse().unwrap());
        assert!(matches!(
            a.send(PeerId(1), &vec![0u8; MTU + 1], Timestamp::ZERO),
            Err(TransportError::PayloadTooLarge { .. })
        ));
    }

    #[test]
    fn receiving_never_blocks_when_nothing_has_arrived() {
        // A blocking read here would couple frame time to network arrival.
        let mut a = bound(0);
        let start = std::time::Instant::now();
        assert!(a.recv(Timestamp::ZERO).is_none());
        assert!(start.elapsed() < std::time::Duration::from_millis(50));
    }

    #[test]
    fn removing_a_peer_makes_it_unaddressable() {
        let mut a = bound(0);
        let addr: SocketAddr = "127.0.0.1:9".parse().unwrap();
        a.add_peer(PeerId(1), addr);
        assert_eq!(a.peer_addr(PeerId(1)), Some(addr));
        a.remove_peer(PeerId(1));
        assert_eq!(a.peer_addr(PeerId(1)), None);
        assert!(a.send(PeerId(1), b"x", Timestamp::ZERO).is_err());
    }
}
