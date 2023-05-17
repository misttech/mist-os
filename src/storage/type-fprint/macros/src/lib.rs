// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use {
    proc_macro2::TokenStream,
    quote::quote,
    syn::{parse_macro_input, Data, DataEnum, DataStruct, DeriveInput, Fields, Generics, Ident},
};

#[proc_macro_derive(TypeFingerprint)]
pub fn derive_type_fingerprint(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    match &input.data {
        Data::Struct(data) => handle_struct(&input.ident, &input.generics, data),
        Data::Enum(data) => handle_enum(&input.ident, &input.generics, data),
        Data::Union(_) => unimplemented!(),
    }
    .into()
}

fn handle_fields(fields: &syn::Fields) -> Vec<TokenStream> {
    match &fields {
        Fields::Unit => vec![],
        Fields::Named(fields) => fields
            .named
            .iter()
            .map(|field| {
                let field_type = &field.ty;
                if let Some(field_name) = &field.ident {
                    let field_name = field_name.to_string();
                    quote! { #field_name + ":" + &(<#field_type as TypeFingerprint>::fingerprint()) }
                } else {
                    quote! { &(<#field_type as TypeFingerprint>::fingerprint()) }
                }
            })
            .collect(),
        Fields::Unnamed(fields) => fields
            .unnamed
            .iter()
            .map(|field| {
                let field_type = &field.ty;
                quote! { &(<#field_type as TypeFingerprint>::fingerprint()) }
            })
            .collect(),
    }
}

fn handle_struct(ident: &Ident, _generics: &Generics, data: &DataStruct) -> TokenStream {
    let fields = handle_fields(&data.fields);
    let mut out = quote! { "" };
    for (i, field) in fields.into_iter().enumerate() {
        if i != 0 {
            out = quote! { #out + "," }
        }
        out = quote! { #out + #field }
    }
    quote! {
        impl TypeFingerprint for #ident {
            fn fingerprint() -> String {
                "struct {".to_owned() + #out + "}"
            }
        }
    }
}

fn handle_enum(ident: &Ident, _generics: &Generics, data: &DataEnum) -> TokenStream {
    let variants = data.variants.iter().map(|variant| {
        let name = variant.ident.to_string();
        let fields = handle_fields(&variant.fields);
        if fields.is_empty() {
            quote! { #name }
        } else {
            let mut out = quote! { #name + "(" };
            for (i, field) in fields.into_iter().enumerate() {
                if i != 0 {
                    out = quote! { #out + "," }
                }
                out = quote! { #out + #field }
            }
            quote! { #out + ")" }
        }
    });
    let mut out = quote! { "" };
    for (i, variant) in variants.into_iter().enumerate() {
        if i != 0 {
            out = quote! { #out + "," }
        }
        out = quote! { #out + #variant }
    }
    quote! {
        impl TypeFingerprint for #ident {
            fn fingerprint() -> String {
                    "enum {".to_owned() + #out + "}"
            }
        }
    }
}
