//! Procedural macros for the SuperDuper DSP SDK.
//!
//! Currently exports:
//! - `params!` — declares effect parameters and emits the stable ABI metadata
//!   the host plugin reads via `sdsp_param_count` / `sdsp_param_meta` after
//!   hot-reload.
//!
//! ```ignore
//! params! {
//!     GAIN  = param(-24.0, 24.0).default(0.0).unit("dB"),
//!     DRIVE = param(0.0, 1.0).default(0.5),
//! }
//! ```
//! Expands to:
//! - `pub const GAIN: usize = 0; pub const DRIVE: usize = 1;`
//! - `pub const __PARAM_COUNT: usize = 2;`
//! - `static __PARAM_NAME_0: &[u8] = b"GAIN\0"; …`
//! - `static __PARAM_METAS: [ParamMeta; 2] = [..];`

use proc_macro::TokenStream;
use proc_macro2::Span;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::{Expr, Ident, LitFloat, LitStr, Result, Token};

/// One entry in `params! { NAME = param(MIN, MAX).default(DEF).unit("U") }`.
struct ParamDecl {
    name: Ident,
    min: f32,
    max: f32,
    default: f32,
    unit: Option<String>,
}

impl Parse for ParamDecl {
    fn parse(input: ParseStream) -> Result<Self> {
        let name: Ident = input.parse()?;
        input.parse::<Token![=]>()?;

        // We accept an arbitrary expression and then walk it to extract
        // `param(min, max)` plus optional `.default(...)` and `.unit("…")`
        // chained calls. Anything else is a syntax error pointed at the call.
        let expr: Expr = input.parse()?;
        let (min, max, default, unit) = parse_builder(&expr)?;

        Ok(Self {
            name,
            min,
            max,
            default,
            unit,
        })
    }
}

/// Walk a `param(MIN, MAX).default(D).unit("U")` chain and pull out the values.
fn parse_builder(mut expr: &Expr) -> Result<(f32, f32, f32, Option<String>)> {
    let mut default: Option<f32> = None;
    let mut unit: Option<String> = None;

    loop {
        match expr {
            Expr::MethodCall(call) => {
                let method = call.method.to_string();
                match method.as_str() {
                    "default" => {
                        let arg = call.args.first().ok_or_else(|| {
                            syn::Error::new(call.method.span(), ".default() takes one f32")
                        })?;
                        default = Some(parse_f32(arg)?);
                    }
                    "unit" => {
                        let arg = call.args.first().ok_or_else(|| {
                            syn::Error::new(call.method.span(), ".unit() takes one string")
                        })?;
                        unit = Some(parse_str(arg)?);
                    }
                    other => {
                        return Err(syn::Error::new(
                            call.method.span(),
                            format!("unknown builder method `.{other}()`"),
                        ));
                    }
                }
                expr = &call.receiver;
            }
            Expr::Call(call) => {
                // Expect `param(min, max)`
                let path = match call.func.as_ref() {
                    Expr::Path(p) => p,
                    _ => {
                        return Err(syn::Error::new_spanned(
                            &call.func,
                            "expected `param(min, max)`",
                        ));
                    }
                };
                let name = path
                    .path
                    .segments
                    .last()
                    .map(|s| s.ident.to_string())
                    .unwrap_or_default();
                if name != "param" {
                    return Err(syn::Error::new_spanned(
                        path,
                        format!("expected `param(min, max)`, got `{name}(...)`"),
                    ));
                }
                if call.args.len() != 2 {
                    return Err(syn::Error::new_spanned(
                        &call.args,
                        "`param(...)` takes exactly two f32 arguments (min, max)",
                    ));
                }
                let min = parse_f32(&call.args[0])?;
                let max = parse_f32(&call.args[1])?;
                let default = default.unwrap_or((min + max) * 0.5);
                return Ok((min, max, default, unit));
            }
            other => {
                return Err(syn::Error::new_spanned(
                    other,
                    "expected `param(min, max).default(d).unit(\"u\")` chain",
                ));
            }
        }
    }
}

fn parse_f32(expr: &Expr) -> Result<f32> {
    match expr {
        Expr::Lit(lit) => match &lit.lit {
            syn::Lit::Float(f) => f.base10_parse::<f32>(),
            syn::Lit::Int(i) => i.base10_parse::<f32>(),
            _ => Err(syn::Error::new_spanned(lit, "expected numeric literal")),
        },
        Expr::Unary(u) if matches!(u.op, syn::UnOp::Neg(_)) => {
            let v = parse_f32(&u.expr)?;
            Ok(-v)
        }
        _ => Err(syn::Error::new_spanned(
            expr,
            "expected a plain f32 literal (e.g. 0.5, -12.0)",
        )),
    }
}

fn parse_str(expr: &Expr) -> Result<String> {
    match expr {
        Expr::Lit(lit) => match &lit.lit {
            syn::Lit::Str(s) => Ok(s.value()),
            _ => Err(syn::Error::new_spanned(lit, "expected a string literal")),
        },
        _ => Err(syn::Error::new_spanned(
            expr,
            "expected a string literal like \"dB\"",
        )),
    }
}

struct ParamsInput {
    decls: Punctuated<ParamDecl, Token![,]>,
}

impl Parse for ParamsInput {
    fn parse(input: ParseStream) -> Result<Self> {
        Ok(Self {
            decls: Punctuated::parse_terminated(input)?,
        })
    }
}

#[proc_macro]
pub fn params(input: TokenStream) -> TokenStream {
    let parsed = syn::parse_macro_input!(input as ParamsInput);
    let count = parsed.decls.len();

    let consts = parsed.decls.iter().enumerate().map(|(i, d)| {
        let name = &d.name;
        quote! { pub const #name: usize = #i; }
    });

    let name_statics = parsed.decls.iter().enumerate().map(|(i, d)| {
        let ident = Ident::new(&format!("__PARAM_NAME_{i}"), Span::call_site());
        let lit = LitStr::new(&format!("{}\0", d.name), Span::call_site());
        quote! { #[doc(hidden)] static #ident: &[u8] = #lit.as_bytes(); }
    });

    let unit_statics = parsed.decls.iter().enumerate().map(|(i, d)| {
        let ident = Ident::new(&format!("__PARAM_UNIT_{i}"), Span::call_site());
        let s = d.unit.clone().unwrap_or_default() + "\0";
        let lit = LitStr::new(&s, Span::call_site());
        quote! { #[doc(hidden)] static #ident: &[u8] = #lit.as_bytes(); }
    });

    let meta_entries = parsed.decls.iter().enumerate().map(|(i, d)| {
        let name_ident = Ident::new(&format!("__PARAM_NAME_{i}"), Span::call_site());
        let unit_ident = Ident::new(&format!("__PARAM_UNIT_{i}"), Span::call_site());
        let min = LitFloat::new(&format!("{:?}f32", d.min), Span::call_site());
        let max = LitFloat::new(&format!("{:?}f32", d.max), Span::call_site());
        let def = LitFloat::new(&format!("{:?}f32", d.default), Span::call_site());
        quote! {
            ::superduper_dsp_sdk::ParamMeta {
                name: #name_ident.as_ptr(),
                min: #min,
                max: #max,
                default: #def,
                unit: #unit_ident.as_ptr(),
            }
        }
    });

    let expanded = quote! {
        #(#consts)*

        #[doc(hidden)]
        pub const __PARAM_COUNT: usize = #count;

        #(#name_statics)*
        #(#unit_statics)*

        #[doc(hidden)]
        #[allow(non_upper_case_globals)]
        pub static __PARAM_METAS: [::superduper_dsp_sdk::ParamMeta; #count] = [
            #(#meta_entries),*
        ];
    };

    expanded.into()
}
