//! `#[derive(Replicate)]` — the native-first schema declaration from
//! [ADR-0003](../../../docs/adr/0003-native-first-schema-derivation.md).
//!
//! A user writes an ordinary Rust struct and marks the fields that replicate. No IDL, no build step,
//! no code generator to install — the whole point of "just import it".
//!
//! ```ignore
//! #[derive(Replicate)]
//! struct Player {
//!     #[replicate(quantize = "0.001", min = "-1000", max = "1000", priority = 2.0)]
//!     position: Vec2,
//!     #[replicate(bits = 10)]
//!     score: u32,
//!     alive: bool,
//! }
//! ```
//!
//! # Floats are resolved at compile time, never at runtime
//!
//! `quantize = "0.001"` is parsed here, during macro expansion, and emitted as the raw `i64` literal
//! `0x418937`. The generated code contains no floating point at all. This keeps the promise in
//! ADR-0002 — floats appear only in offline generation — and matches the canonical schema form,
//! which renders `Fx` parameters as raw integers precisely so that float formatting can never differ
//! between languages.

use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{parse_macro_input, Attribute, Data, DeriveInput, Expr, Fields, Lit, Meta, Type};

/// Derives a replicated component from a struct.
///
/// Generates an implementation describing the component's schema and converting between the struct
/// and the world arena.
#[proc_macro_derive(Replicate, attributes(replicate))]
pub fn derive_replicate(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match expand(input) {
        Ok(ts) => ts.into(),
        Err(e) => e.to_compile_error().into(),
    }
}

/// Everything declared about one field.
struct FieldSpec {
    ident: syn::Ident,
    ty: Type,
    quantize: Option<String>,
    min: Option<String>,
    max: Option<String>,
    bits: Option<u32>,
    quantize_bits: Option<u32>,
    variants: Option<u32>,
    max_len: Option<u32>,
    priority: Option<f32>,
    skip: bool,
}

fn expand(input: DeriveInput) -> syn::Result<proc_macro2::TokenStream> {
    let name = &input.ident;
    let name_str = name.to_string();

    let Data::Struct(data) = &input.data else {
        return Err(syn::Error::new_spanned(
            &input,
            "Replicate can only be derived for structs; an enum has no fields to replicate",
        ));
    };
    let Fields::Named(named) = &data.fields else {
        return Err(syn::Error::new_spanned(
            &input,
            "Replicate needs named fields — the wire format addresses fields by name, so a tuple \
             struct has nothing to agree on",
        ));
    };

    let mut specs = Vec::new();
    for field in &named.named {
        let ident = field.ident.clone().expect("named fields have identifiers");
        let mut spec = FieldSpec {
            ident,
            ty: field.ty.clone(),
            quantize: None,
            min: None,
            max: None,
            bits: None,
            quantize_bits: None,
            variants: None,
            max_len: None,
            priority: None,
            skip: false,
        };
        parse_field_attrs(&field.attrs, &mut spec)?;
        specs.push(spec);
    }
    let (specs, skipped): (Vec<FieldSpec>, Vec<FieldSpec>) =
        specs.into_iter().partition(|s| !s.skip);

    if specs.is_empty() {
        return Err(syn::Error::new_spanned(
            &input,
            "a replicated component needs at least one field that is not skipped",
        ));
    }

    let mut descriptors = Vec::new();
    for s in &specs {
        descriptors.push(field_descriptor(s)?);
    }

    // Reads and writes go through the *canonical* index, which the runtime resolves by name. Using
    // declaration order here would break the moment a user reordered their struct.
    let field_names: Vec<String> = specs.iter().map(|s| s.ident.to_string()).collect();
    let field_idents: Vec<&syn::Ident> = specs.iter().map(|s| &s.ident).collect();
    let to_value: Vec<proc_macro2::TokenStream> = specs
        .iter()
        .map(value_from_field)
        .collect::<syn::Result<_>>()?;
    let from_value: Vec<proc_macro2::TokenStream> = specs
        .iter()
        .map(field_from_value)
        .collect::<syn::Result<_>>()?;

    // A skipped field is not replicated, so reading has nothing to reconstruct it from. It comes
    // back as its type's default — which is why skipping requires `Default`, and why the
    // requirement lands on the individual field's type rather than the whole struct.
    let skipped_idents: Vec<&syn::Ident> = skipped.iter().map(|s| &s.ident).collect();
    let skipped_types: Vec<&Type> = skipped.iter().map(|s| &s.ty).collect();

    Ok(quote! {
        impl ::tempo::Replicate for #name {
            const COMPONENT_NAME: &'static str = #name_str;

            fn describe() -> ::tempo::ComponentDesc {
                ::tempo::ComponentDesc::new(#name_str, ::std::vec![ #(#descriptors),* ])
            }

            fn write_into(
                &self,
                world: &mut ::tempo::World,
                entity: ::tempo::Entity,
                component: ::tempo::ComponentId,
            ) -> ::core::result::Result<(), ::tempo::CoreError> {
                #(
                    world.set_named(entity, component, #field_names, &#to_value)?;
                )*
                ::core::result::Result::Ok(())
            }

            fn read_from(
                world: &::tempo::World,
                entity: ::tempo::Entity,
                component: ::tempo::ComponentId,
            ) -> ::core::result::Result<Self, ::tempo::CoreError> {
                #(
                    let #field_idents = {
                        let value = world.get_named(entity, component, #field_names)?;
                        #from_value
                    };
                )*
                #(
                    let #skipped_idents = <#skipped_types as ::core::default::Default>::default();
                )*
                ::core::result::Result::Ok(Self {
                    #(#field_idents,)*
                    #(#skipped_idents,)*
                })
            }
        }
    })
}

fn parse_field_attrs(attrs: &[Attribute], spec: &mut FieldSpec) -> syn::Result<()> {
    for attr in attrs {
        if !attr.path().is_ident("replicate") {
            continue;
        }
        let nested = attr.parse_args_with(
            syn::punctuated::Punctuated::<Meta, syn::Token![,]>::parse_terminated,
        )?;
        for meta in nested {
            match &meta {
                Meta::Path(p) if p.is_ident("skip") => spec.skip = true,
                Meta::NameValue(nv) => {
                    let key = nv
                        .path
                        .get_ident()
                        .map(|i| i.to_string())
                        .unwrap_or_default();
                    match key.as_str() {
                        "quantize" => spec.quantize = Some(expect_str(&nv.value)?),
                        "min" => spec.min = Some(expect_str(&nv.value)?),
                        "max" => spec.max = Some(expect_str(&nv.value)?),
                        "bits" => spec.bits = Some(expect_int(&nv.value)? as u32),
                        "quantize_bits" => spec.quantize_bits = Some(expect_int(&nv.value)? as u32),
                        "variants" => spec.variants = Some(expect_int(&nv.value)? as u32),
                        "max_len" => spec.max_len = Some(expect_int(&nv.value)? as u32),
                        "priority" => spec.priority = Some(expect_float(&nv.value)? as f32),
                        other => {
                            return Err(syn::Error::new_spanned(
                                &nv.path,
                                format!(
                                    "unknown replicate option `{other}`; expected one of \
                                     quantize, min, max, bits, quantize_bits, variants, max_len, \
                                     priority, skip"
                                ),
                            ))
                        }
                    }
                }
                _ => {
                    return Err(syn::Error::new_spanned(
                        &meta,
                        "expected `name = value` or `skip`",
                    ))
                }
            }
        }
    }

    // Catching this here rather than at runtime means the error points at the field.
    let declared = [
        spec.quantize.is_some(),
        spec.min.is_some(),
        spec.max.is_some(),
    ];
    if declared.iter().any(|d| *d) && !declared.iter().all(|d| *d) {
        return Err(syn::Error::new_spanned(
            &spec.ty,
            "quantize, min and max must be given together: quantization needs a range to divide",
        ));
    }
    Ok(())
}

fn expect_str(e: &Expr) -> syn::Result<String> {
    match e {
        Expr::Lit(l) => match &l.lit {
            Lit::Str(s) => Ok(s.value()),
            Lit::Int(i) => Ok(i.base10_digits().to_string()),
            Lit::Float(f) => Ok(f.base10_digits().to_string()),
            other => Err(syn::Error::new_spanned(other, "expected a string literal")),
        },
        other => Err(syn::Error::new_spanned(other, "expected a literal")),
    }
}

fn expect_int(e: &Expr) -> syn::Result<u64> {
    match e {
        Expr::Lit(l) => match &l.lit {
            Lit::Int(i) => i.base10_parse::<u64>(),
            other => Err(syn::Error::new_spanned(
                other,
                "expected an integer literal",
            )),
        },
        other => Err(syn::Error::new_spanned(
            other,
            "expected an integer literal",
        )),
    }
}

fn expect_float(e: &Expr) -> syn::Result<f64> {
    match e {
        Expr::Lit(l) => match &l.lit {
            Lit::Float(f) => f.base10_parse::<f64>(),
            Lit::Int(i) => Ok(i.base10_parse::<i64>()? as f64),
            other => Err(syn::Error::new_spanned(other, "expected a number")),
        },
        other => Err(syn::Error::new_spanned(other, "expected a number")),
    }
}

/// Converts a decimal string to a raw Q32.32 value, at macro expansion time.
///
/// Rounds half away from zero, matching `docs/spec/fixed-point.md` §1.4. The result is emitted as an
/// integer literal, so no float survives into the generated code.
fn fx_raw(s: &str, span: proc_macro2::Span) -> syn::Result<i64> {
    let v: f64 = s
        .trim()
        .parse()
        .map_err(|_| syn::Error::new(span, format!("`{s}` is not a number")))?;
    let scaled = v * 4_294_967_296.0;
    if !scaled.is_finite() {
        return Err(syn::Error::new(
            span,
            format!("`{s}` is out of range for a fixed-point value"),
        ));
    }
    let rounded = if scaled >= 0.0 {
        (scaled + 0.5).floor()
    } else {
        (scaled - 0.5).ceil()
    };
    if rounded >= i64::MAX as f64 || rounded <= i64::MIN as f64 {
        return Err(syn::Error::new(
            span,
            format!("`{s}` is out of range for a fixed-point value"),
        ));
    }
    Ok(rounded as i64)
}

/// The `FieldType` a Rust type maps to.
fn field_type_of(ty: &Type) -> Option<&'static str> {
    let Type::Path(p) = ty else { return None };
    let last = p.path.segments.last()?;
    Some(match last.ident.to_string().as_str() {
        "bool" => "Bool",
        "Fx" => "Fx",
        "Vec2" => "Vec2",
        "Vec3" => "Vec3",
        "Quat" => "Quat",
        "u8" | "u16" | "u32" | "u64" | "usize" => "Uint",
        "i8" | "i16" | "i32" | "i64" | "isize" => "Int",
        _ => return None,
    })
}

fn field_descriptor(s: &FieldSpec) -> syn::Result<proc_macro2::TokenStream> {
    let name = s.ident.to_string();
    let Some(kind) = field_type_of(&s.ty) else {
        return Err(syn::Error::new_spanned(
            &s.ty,
            "this type cannot be replicated. Supported: bool, u8..u64, i8..i64, Fx, Vec2, Vec3, \
             Quat. String and Vec<u8> are deliberately excluded — variable length would break the \
             flat snapshot copy that rollback depends on (ADR-0027). Use #[replicate(skip)] to \
             keep the field local, or send it as a command.",
        ));
    };
    let kind_ident = format_ident!("{}", kind);

    let mut builder = quote! {
        ::tempo::FieldDesc::new(#name, ::tempo::FieldType::#kind_ident)
    };

    if let (Some(q), Some(lo), Some(hi)) = (&s.quantize, &s.min, &s.max) {
        let span = s.ident.span();
        let (qr, lr, hr) = (fx_raw(q, span)?, fx_raw(lo, span)?, fx_raw(hi, span)?);
        if qr <= 0 {
            return Err(syn::Error::new(span, "quantize must be greater than zero"));
        }
        if lr >= hr {
            return Err(syn::Error::new(span, "min must be less than max"));
        }
        builder = quote! {
            #builder.with_quantize(
                ::tempo::Fx::from_raw(#qr),
                ::tempo::Fx::from_raw(#lr),
                ::tempo::Fx::from_raw(#hr),
            )
        };
    }
    if let Some(b) = s.bits {
        builder = quote! { #builder.with_bits(#b) };
    }
    if let Some(b) = s.quantize_bits {
        builder = quote! { #builder.with_quantize_bits(#b) };
    }
    if let Some(v) = s.variants {
        builder = quote! { #builder.with_variants(#v) };
    }
    if let Some(m) = s.max_len {
        builder = quote! { #builder.with_max_len(#m) };
    }
    if let Some(p) = s.priority {
        builder = quote! { #builder.with_priority(#p) };
    }
    Ok(builder)
}

fn value_from_field(s: &FieldSpec) -> syn::Result<proc_macro2::TokenStream> {
    let ident = &s.ident;
    let kind = field_type_of(&s.ty).expect("checked in field_descriptor");
    Ok(match kind {
        "Bool" => quote! { ::tempo::Value::Bool(self.#ident) },
        "Fx" => quote! { ::tempo::Value::Fx(self.#ident) },
        "Vec2" => quote! { ::tempo::Value::Vec2(self.#ident) },
        "Vec3" => quote! { ::tempo::Value::Vec3(self.#ident) },
        "Quat" => quote! { ::tempo::Value::Quat(self.#ident) },
        "Uint" => quote! { ::tempo::Value::Uint(self.#ident as u64) },
        _ => quote! { ::tempo::Value::Int(self.#ident as i64) },
    })
}

fn field_from_value(s: &FieldSpec) -> syn::Result<proc_macro2::TokenStream> {
    let ty = &s.ty;
    let kind = field_type_of(&s.ty).expect("checked in field_descriptor");
    let name = s.ident.to_string();
    let variant = format_ident!("{}", kind);

    // Numeric fields narrow back to the declared Rust type; the others are already exact.
    let extract = match kind {
        "Uint" | "Int" => quote! { v as #ty },
        _ => quote! { v },
    };

    Ok(quote! {
        match value {
            ::tempo::Value::#variant(v) => #extract,
            other => {
                return ::core::result::Result::Err(::tempo::CoreError::UnknownFieldName(
                    ::std::format!(
                        "{}: expected {}, arena held {:?}",
                        #name,
                        ::core::stringify!(#variant),
                        other,
                    ),
                ))
            }
        }
    })
}
