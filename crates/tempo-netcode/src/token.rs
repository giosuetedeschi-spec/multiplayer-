//! Connect tokens.
//!
//! Implements the scheme in [ADR-0015](../../../docs/adr/0015-connect-tokens-and-security.md),
//! following netcode.io's design. The shape of it is the point:
//!
//! 1. The client authenticates with **the game's own backend**, however that game already does it.
//! 2. The backend — which shares a private key with the game servers — issues a connect token.
//! 3. The client presents the token to a game server, which validates it **offline**.
//!
//! So the game server never sees credentials, never queries an auth database on the connect path,
//! and keeps working when the auth service does not. That last property is the reason to do it this
//! way rather than checking a session token against an API on every connection.
//!
//! # Two halves
//!
//! A token has a public half the client reads and a private half only servers can open. The client
//! needs the session keys to talk to the server; the server needs the client's identity. Both are in
//! the private half, and the public half repeats the keys for the client's benefit — which is safe
//! precisely because the public half travels over the backend's already-authenticated channel
//! (HTTPS), never over the game transport.

use std::net::SocketAddr;

use crate::crypto::{open, seal, Key, TAG_BYTES};
use crate::NetcodeError;

/// Bytes of user data a token may carry.
///
/// Fixed rather than variable so that every token is the same size: a token whose length varied
/// with its contents would leak how much data a given player carries.
pub const USER_DATA_BYTES: usize = 256;

/// Default validity of a freshly issued token, in seconds.
///
/// Deliberately short. A stolen token is a valid credential until it expires, so the window is
/// sized for "connect now", not for "connect at some point today".
pub const DEFAULT_EXPIRY_SECONDS: u64 = 30;

/// The half of a token only a game server can open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrivateToken {
    /// The authenticated player.
    pub client_id: u64,
    /// Opaque application data — team, party, entitlements — chosen by the backend.
    pub user_data: Vec<u8>,
    /// Key for traffic from client to server.
    pub client_to_server_key: Key,
    /// Key for traffic from server to client.
    pub server_to_client_key: Key,
}

impl PrivateToken {
    fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(8 + USER_DATA_BYTES + 64);
        out.extend_from_slice(&self.client_id.to_le_bytes());
        let mut user = [0u8; USER_DATA_BYTES];
        let n = self.user_data.len().min(USER_DATA_BYTES);
        user[..n].copy_from_slice(&self.user_data[..n]);
        out.extend_from_slice(&user);
        out.extend_from_slice(self.client_to_server_key.as_bytes());
        out.extend_from_slice(self.server_to_client_key.as_bytes());
        out
    }

    fn decode(b: &[u8]) -> Result<PrivateToken, NetcodeError> {
        const LEN: usize = 8 + USER_DATA_BYTES + 32 + 32;
        if b.len() != LEN {
            return Err(NetcodeError::MalformedToken);
        }
        let client_id = u64::from_le_bytes(b[0..8].try_into().expect("checked length"));
        let user_data = b[8..8 + USER_DATA_BYTES].to_vec();
        let mut c2s = [0u8; 32];
        let mut s2c = [0u8; 32];
        c2s.copy_from_slice(&b[8 + USER_DATA_BYTES..8 + USER_DATA_BYTES + 32]);
        s2c.copy_from_slice(&b[8 + USER_DATA_BYTES + 32..]);
        Ok(PrivateToken {
            client_id,
            user_data,
            client_to_server_key: Key::from_bytes(c2s),
            server_to_client_key: Key::from_bytes(s2c),
        })
    }
}

/// A connect token as delivered to the client by the backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectToken {
    /// Distinguishes this game and protocol version from any other using the same servers.
    pub protocol_id: u64,
    /// Unix seconds after which servers must refuse this token.
    pub expire_at: u64,
    /// Unique per token; also the AEAD nonce for the private half.
    pub nonce: [u8; 24],
    /// The private half, sealed under the shared key.
    pub sealed_private: Vec<u8>,
    /// Servers this token is valid for.
    pub server_addresses: Vec<SocketAddr>,
    /// Key for traffic from client to server.
    pub client_to_server_key: Key,
    /// Key for traffic from server to client.
    pub server_to_client_key: Key,
}

/// Associated data binding a sealed token to its protocol and expiry.
///
/// Without this the two fields travel in the clear and unauthenticated, so anyone could extend a
/// captured token's lifetime or retarget it at a different protocol version.
fn token_aad(protocol_id: u64, expire_at: u64) -> Vec<u8> {
    let mut aad = Vec::with_capacity(16);
    aad.extend_from_slice(&protocol_id.to_le_bytes());
    aad.extend_from_slice(&expire_at.to_le_bytes());
    aad
}

impl ConnectToken {
    /// Issues a token. Called by the backend, which holds `private_key`.
    ///
    /// `now_unix` is passed rather than read from the clock so that issuance is testable and
    /// replayable, consistent with the rest of the engine.
    pub fn issue(
        private_key: &Key,
        protocol_id: u64,
        client_id: u64,
        user_data: &[u8],
        server_addresses: Vec<SocketAddr>,
        now_unix: u64,
        expiry_seconds: u64,
    ) -> Result<ConnectToken, NetcodeError> {
        if server_addresses.is_empty() {
            return Err(NetcodeError::MalformedToken);
        }
        if user_data.len() > USER_DATA_BYTES {
            return Err(NetcodeError::MalformedToken);
        }

        let client_to_server_key = Key::generate()?;
        let server_to_client_key = Key::generate()?;
        let mut nonce = [0u8; 24];
        getrandom::fill(&mut nonce).map_err(|e| NetcodeError::Entropy(e.to_string()))?;

        let expire_at = now_unix.saturating_add(expiry_seconds);
        let private = PrivateToken {
            client_id,
            user_data: user_data.to_vec(),
            client_to_server_key,
            server_to_client_key,
        };

        // The nonce is 24 bytes for uniqueness across a very large number of tokens; the packet
        // cipher takes 96 bits, so the first 8 bytes seed the sequence-shaped nonce here.
        let seq = u64::from_le_bytes(nonce[..8].try_into().expect("24 bytes available"));
        let sealed_private = seal(
            private_key,
            seq,
            &token_aad(protocol_id, expire_at),
            &private.encode(),
        )?;

        Ok(ConnectToken {
            protocol_id,
            expire_at,
            nonce,
            sealed_private,
            server_addresses,
            client_to_server_key,
            server_to_client_key,
        })
    }

    /// Opens the private half. Called by a game server, which holds `private_key`.
    ///
    /// Validates expiry, protocol and authenticity without contacting the backend, which is what
    /// keeps game servers available when the backend is not.
    pub fn open_private(
        &self,
        private_key: &Key,
        expected_protocol: u64,
        now_unix: u64,
    ) -> Result<PrivateToken, NetcodeError> {
        if self.protocol_id != expected_protocol {
            return Err(NetcodeError::ProtocolMismatch {
                expected: expected_protocol,
                found: self.protocol_id,
            });
        }
        if now_unix >= self.expire_at {
            return Err(NetcodeError::TokenExpired {
                expired_at: self.expire_at,
                now: now_unix,
            });
        }
        let seq = u64::from_le_bytes(self.nonce[..8].try_into().expect("24 bytes available"));
        let plain = open(
            private_key,
            seq,
            &token_aad(self.protocol_id, self.expire_at),
            &self.sealed_private,
        )?;
        PrivateToken::decode(&plain)
    }

    /// The bytes a client sends to a game server.
    ///
    /// Only the public fields and the sealed private half — never the plaintext session keys, which
    /// stay on the client.
    pub fn to_wire(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(64 + self.sealed_private.len());
        out.extend_from_slice(&self.protocol_id.to_le_bytes());
        out.extend_from_slice(&self.expire_at.to_le_bytes());
        out.extend_from_slice(&self.nonce);
        out.extend_from_slice(&(self.sealed_private.len() as u32).to_le_bytes());
        out.extend_from_slice(&self.sealed_private);
        out
    }

    /// Parses the bytes a client sent.
    ///
    /// The result carries no session keys: a server learns them by opening the private half, and a
    /// token parsed from the wire must never be mistaken for one issued by the backend.
    pub fn from_wire(bytes: &[u8]) -> Result<ConnectToken, NetcodeError> {
        const HEADER: usize = 8 + 8 + 24 + 4;
        if bytes.len() < HEADER {
            return Err(NetcodeError::MalformedToken);
        }
        let protocol_id = u64::from_le_bytes(bytes[0..8].try_into().expect("checked"));
        let expire_at = u64::from_le_bytes(bytes[8..16].try_into().expect("checked"));
        let mut nonce = [0u8; 24];
        nonce.copy_from_slice(&bytes[16..40]);
        let len = u32::from_le_bytes(bytes[40..44].try_into().expect("checked")) as usize;

        // Validate the declared length against what is actually present before allocating.
        if len > bytes.len() - HEADER || len < TAG_BYTES {
            return Err(NetcodeError::MalformedToken);
        }
        Ok(ConnectToken {
            protocol_id,
            expire_at,
            nonce,
            sealed_private: bytes[HEADER..HEADER + len].to_vec(),
            server_addresses: Vec::new(),
            client_to_server_key: Key::from_bytes([0u8; 32]),
            server_to_client_key: Key::from_bytes([0u8; 32]),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PROTOCOL: u64 = 0xABCD;
    const NOW: u64 = 1_700_000_000;

    fn addrs() -> Vec<SocketAddr> {
        vec!["127.0.0.1:7777".parse().unwrap()]
    }

    fn issue(key: &Key, now: u64) -> ConnectToken {
        ConnectToken::issue(
            key,
            PROTOCOL,
            42,
            b"team=red",
            addrs(),
            now,
            DEFAULT_EXPIRY_SECONDS,
        )
        .expect("issue")
    }

    #[test]
    fn a_server_opens_a_token_without_contacting_the_backend() {
        let key = Key::generate().unwrap();
        let token = issue(&key, NOW);
        let private = token.open_private(&key, PROTOCOL, NOW + 1).unwrap();

        assert_eq!(private.client_id, 42);
        assert_eq!(&private.user_data[..8], b"team=red");
        // The client and server must derive the same session keys.
        assert_eq!(private.client_to_server_key, token.client_to_server_key);
        assert_eq!(private.server_to_client_key, token.server_to_client_key);
    }

    #[test]
    fn a_token_from_another_backend_is_refused() {
        let token = issue(&Key::generate().unwrap(), NOW);
        let other = Key::generate().unwrap();
        assert!(matches!(
            token.open_private(&other, PROTOCOL, NOW + 1),
            Err(NetcodeError::Crypto)
        ));
    }

    #[test]
    fn expired_tokens_are_refused() {
        let key = Key::generate().unwrap();
        let token = issue(&key, NOW);
        assert!(token.open_private(&key, PROTOCOL, NOW + 29).is_ok());
        assert!(matches!(
            token.open_private(&key, PROTOCOL, NOW + DEFAULT_EXPIRY_SECONDS),
            Err(NetcodeError::TokenExpired { .. })
        ));
    }

    #[test]
    fn a_token_for_another_protocol_is_refused() {
        let key = Key::generate().unwrap();
        let token = issue(&key, NOW);
        assert!(matches!(
            token.open_private(&key, 0x9999, NOW + 1),
            Err(NetcodeError::ProtocolMismatch { .. })
        ));
    }

    #[test]
    fn the_expiry_cannot_be_extended_by_tampering() {
        // Expiry travels in the clear, so it must be authenticated as associated data. Otherwise a
        // stolen token could be given an arbitrary lifetime.
        let key = Key::generate().unwrap();
        let mut token = issue(&key, NOW);
        token.expire_at += 100_000;
        assert!(matches!(
            token.open_private(&key, PROTOCOL, NOW + 1),
            Err(NetcodeError::Crypto)
        ));
    }

    #[test]
    fn the_protocol_id_cannot_be_retargeted_by_tampering() {
        let key = Key::generate().unwrap();
        let mut token = issue(&key, NOW);
        token.protocol_id = 0x1234;
        assert!(matches!(
            token.open_private(&key, 0x1234, NOW + 1),
            Err(NetcodeError::Crypto)
        ));
    }

    #[test]
    fn a_modified_sealed_half_is_refused() {
        let key = Key::generate().unwrap();
        let mut token = issue(&key, NOW);
        token.sealed_private[0] ^= 1;
        assert!(matches!(
            token.open_private(&key, PROTOCOL, NOW + 1),
            Err(NetcodeError::Crypto)
        ));
    }

    #[test]
    fn every_token_gets_fresh_keys_and_a_fresh_nonce() {
        // Reusing session keys across tokens would let one player's captured traffic be decrypted
        // with another's keys.
        let key = Key::generate().unwrap();
        let a = issue(&key, NOW);
        let b = issue(&key, NOW);
        assert_ne!(a.client_to_server_key, b.client_to_server_key);
        assert_ne!(a.server_to_client_key, b.server_to_client_key);
        assert_ne!(a.nonce, b.nonce);
    }

    #[test]
    fn the_wire_form_never_carries_the_session_keys() {
        // The public half reaches the client over HTTPS; only the sealed half goes over the game
        // transport. A key appearing in the wire bytes would be a total compromise.
        let key = Key::generate().unwrap();
        let token = issue(&key, NOW);
        let wire = token.to_wire();
        assert!(
            !wire
                .windows(32)
                .any(|w| w == token.client_to_server_key.as_bytes()),
            "the client-to-server key leaked into the wire form"
        );
        assert!(
            !wire
                .windows(32)
                .any(|w| w == token.server_to_client_key.as_bytes()),
            "the server-to-client key leaked into the wire form"
        );
    }

    #[test]
    fn the_wire_form_round_trips_and_still_opens() {
        let key = Key::generate().unwrap();
        let token = issue(&key, NOW);
        let parsed = ConnectToken::from_wire(&token.to_wire()).unwrap();
        assert_eq!(parsed.protocol_id, token.protocol_id);
        assert_eq!(parsed.expire_at, token.expire_at);
        assert_eq!(parsed.nonce, token.nonce);

        let private = parsed.open_private(&key, PROTOCOL, NOW + 1).unwrap();
        assert_eq!(private.client_id, 42);
    }

    #[test]
    fn malformed_wire_input_is_rejected_rather_than_trusted() {
        assert!(ConnectToken::from_wire(&[]).is_err());
        assert!(ConnectToken::from_wire(&[0u8; 43]).is_err());

        // A declared length larger than the data present must not drive an allocation.
        let mut bytes = vec![0u8; 44 + 8];
        bytes[40..44].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(matches!(
            ConnectToken::from_wire(&bytes),
            Err(NetcodeError::MalformedToken)
        ));
    }

    #[test]
    fn issuing_requires_at_least_one_server_and_bounded_user_data() {
        let key = Key::generate().unwrap();
        assert!(ConnectToken::issue(&key, PROTOCOL, 1, b"", vec![], NOW, 30).is_err());
        assert!(ConnectToken::issue(
            &key,
            PROTOCOL,
            1,
            &vec![0u8; USER_DATA_BYTES + 1],
            addrs(),
            NOW,
            30
        )
        .is_err());
    }

    #[test]
    fn user_data_is_padded_so_token_size_does_not_leak_its_contents() {
        let key = Key::generate().unwrap();
        let small = ConnectToken::issue(&key, PROTOCOL, 1, b"a", addrs(), NOW, 30).unwrap();
        let large = ConnectToken::issue(&key, PROTOCOL, 1, &[9u8; 200], addrs(), NOW, 30).unwrap();
        assert_eq!(small.to_wire().len(), large.to_wire().len());
    }
}
