// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use quote::{quote, quote_spanned};
use std::io::Read;
use syn::parse_macro_input;
use syn::spanned::Spanned;

// Turn a CamelCase string into a UPPER_SNAKE_CASE syn::Ident
fn make_ident(mut s: &str, span: proc_macro2::Span) -> syn::Ident {
    let mut uppercase = String::with_capacity(s.len());
    while !s.is_empty() {
        let (chunk, next) = s[1..]
            .find(|c: char| c.is_uppercase())
            .map(|idx| s.split_at(idx + 1))
            .unwrap_or((s, ""));
        s = next;
        if !uppercase.is_empty() {
            uppercase.push('_');
        }
        uppercase.push_str(&chunk.to_uppercase());
    }
    syn::Ident::new(&uppercase, span)
}

/// The FIDL bindings for protocols don't report ordinal numbers, which we need
/// to use for some of our manual protocol dispatching. This macro parses FIDL
/// IR for the FDomain protocol and produces a module named `ordinals` that has
/// a constant containing the ordinal for each method in the protocol.
///
/// The macro takes a single argument: the name of an environment variable. The
/// build system should put the path to the FIDL IR file in that environment
/// variable when it runs the compiler. This is a pretty standard way for the
/// build system to interact with code at compile time in the Rust world.
#[proc_macro]
pub fn extract_ordinals_env(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let input2: proc_macro2::TokenStream = input.clone().into();
    let env_name = parse_macro_input!(input as syn::LitStr);
    let env_name = env_name.value();
    let Ok(path) = std::env::var(&env_name) else {
        let e = format!("Environment variable '{env_name}' is undefined");
        return quote_spanned! {input2.span()=> compile_error!(#e);}.into();
    };

    let mut file = match std::fs::File::open(&path) {
        Ok(file) => file,
        Err(e) => {
            let e = format!(
                "Could not open {path} from {}: {e:?}",
                std::env::current_dir().unwrap().display()
            );
            return quote_spanned! {input2.span()=> compile_error!(#e);}.into();
        }
    };

    let mut ir = String::new();
    if let Err(e) = file.read_to_string(&mut ir) {
        let e = format!("Could not read {path}: {e:?}");
        return quote_spanned! {input2.span()=> compile_error!(#e);}.into();
    }

    let ir: serde_json::Value = serde_json::from_str(&ir).expect("Could not parse FIDL IR!");
    let protocol_declarations =
        ir.get("protocol_declarations").expect("FIDL IR contained no protocol declarations!");
    let protocol_declarations =
        protocol_declarations.as_array().expect("FIDL IR protocol declarations weren't a list!");
    let fdomain_declaration = protocol_declarations
        .iter()
        .find(|x| {
            x.get("name").and_then(serde_json::Value::as_str) == Some("fuchsia.fdomain/FDomain")
        })
        .expect("FIDL IR missing FDomain protocol declaration!");
    let methods =
        fdomain_declaration.get("methods").expect("FIDL IR declared no methods for FDomain");
    let methods = methods.as_array().expect("FIDL IR methods list wasn't a list!");

    let items = methods
        .iter()
        .filter_map(|x| {
            let k = x.get("ordinal").and_then(serde_json::Value::as_u64);
            let v = x
                .get("name")
                .and_then(serde_json::Value::as_str)
                .map(|s| make_ident(s, input2.span()));
            k.and_then(|k| v.map(|v| (k, v)))
        })
        .map(|(ordinal, name)| {
            quote! {
                pub const #name : u64 = #ordinal;
            }
        })
        .collect::<proc_macro2::TokenStream>();

    quote! {
        mod ordinals {
            #items
        }
    }
    .into()
}
