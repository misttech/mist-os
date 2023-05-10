// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! The following proc macros are related to code under src/storage/fxfs/src/serialized_types.

#[macro_use]
extern crate quote;

use {
    proc_macro::TokenStream,
    quote::ToTokens,
    std::collections::BTreeMap,
    syn::{
        parse::{Parse, ParseStream},
        parse_macro_input, Data, Fields, Result,
    },
};

/// Holds an open-ended version range like `3..` meaning version 3 and up.
#[derive(Clone)]
struct PatOpenVersionRange {
    lo: syn::LitInt,
    dots: syn::token::Dot2,
}
impl PatOpenVersionRange {
    fn lo_value(&self) -> u32 {
        self.lo.base10_parse::<u32>().unwrap_or(0)
    }
}
impl std::cmp::PartialEq for PatOpenVersionRange {
    fn eq(&self, other: &Self) -> bool {
        self.lo_value() == other.lo_value()
    }
}
impl std::cmp::PartialOrd for PatOpenVersionRange {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        self.lo_value().partial_cmp(&other.lo_value())
    }
}
impl Parse for PatOpenVersionRange {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        Ok(Self { lo: input.parse()?, dots: input.parse()? })
    }
}
impl ToTokens for PatOpenVersionRange {
    fn to_tokens(&self, tokens: &mut proc_macro2::TokenStream) {
        self.lo.to_tokens(tokens);
        self.dots.to_tokens(tokens);
    }
}

/// Holds a "fat arrow" mapping from version range to type. e.g. `3.. => FooV3,`
struct Arm {
    pat: PatOpenVersionRange,
    _fat_arrow_token: syn::token::FatArrow,
    ident: syn::Ident,
}
impl Parse for Arm {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        Ok(Self { pat: input.parse()?, _fat_arrow_token: input.parse()?, ident: input.parse()? })
    }
}

struct Input {
    arms: Vec<Arm>,
}
impl Parse for Input {
    fn parse(input: ParseStream<'_>) -> Result<Self> {
        Ok(Self {
            arms: input
                .parse_terminated::<Arm, syn::token::Comma>(Arm::parse)?
                .into_iter()
                .collect(),
        })
    }
}

/// Implements traits for versioned structures.
/// Namely:
///   * [Versioned] for all versions.
///   * [VersionedLatest] for the most recent version.
///   * Transitive [From] for any version to a newer version.
#[proc_macro]
pub fn versioned_type(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as Input);
    let arms = input.arms;

    let versions: BTreeMap<u32, syn::Ident> =
        arms.iter().map(|x| (x.pat.lo_value(), x.ident.clone())).collect();
    assert_eq!(arms.len(), versions.len(), "Duplicate version range found.");

    let mut out = quote! {};

    // The latest version should implement VersionedLatest.
    if let Some((major, ident)) = versions.iter().last() {
        // Generate some static assertions up front to check the versions are sensible.
        let assertions = arms.iter().map(|x| x.pat.lo_value()).map(|v| {
            quote! {
                static_assertions::const_assert!(#v <= LATEST_VERSION.major);
            }
        });
        // We order versions in their original match order.
        let match_iter = arms.iter().map(|x| (x.pat.lo_value(), x.ident.clone())).map(|(v, i)| {
            quote! { #v.. => Ok(#i::deserialize_from(reader, version)?.into()), }
        });
        out = quote! {
            #out
            impl VersionedLatest for #ident {
                fn deserialize_from_version<R>(
                    reader: &mut R, version: Version) -> anyhow::Result<Self>
                where R: std::io::Read, Self: Sized {
                    #(#assertions)*
                    assert!(#major <= LATEST_VERSION.major,
                        "Found version > LATEST_VERSION for {}.", stringify!(#ident));
                    const future_ver : u32 = LATEST_VERSION.major + 1;
                    match version.major {
                        future_ver.. => anyhow::bail!(format!(
                                "Invalid future version {} > {} deserializing {}.",
                                version, LATEST_VERSION, stringify!(#ident))),
                        #(#match_iter)*
                        x => anyhow::bail!(format!(
                                "Unsupported version {} for {}.", x, stringify!(#ident))),
                    }
                }
            }
        };
    }

    // The [From] ladder is a little tricky because users are free to repeat types
    // in their version mapping. We use our sequence of versions.
    let mut idents: Vec<&syn::Ident> = versions.values().collect();
    idents.dedup();
    let last_ident = idents.pop().unwrap();

    for i in 0..idents.len().saturating_sub(1) {
        let ident = &idents[i];
        let next_ident = &idents[i + 1];
        out = quote! {
            #out
            impl From<#ident> for #last_ident {
                fn from(item: #ident) -> Self {
                    let tmp: #next_ident = item.into();
                    tmp.into()
                }
            }
        }
    }
    TokenStream::from(out)
}

/// This is just a shorthand for `impl Versioned for Foo {}` which is required for all top-level
/// versioned struct/enum in Fxfs.
#[proc_macro_derive(Versioned)]
pub fn derive_versioned(input: TokenStream) -> TokenStream {
    let syn::DeriveInput { ident, .. } = parse_macro_input!(input);
    TokenStream::from(quote! { impl Versioned for #ident {} })
}

/// This does nothing by itself, but with the Migrate derive macro, it signals that the struct being
/// migrated to, does not require a Default implementation, since all the fields are available in
/// both.
#[proc_macro_attribute]
pub fn migrate_nodefault(_attr: TokenStream, item: TokenStream) -> TokenStream {
    item
}

/// This does nothing by itself, but with the Migrate derive macro, it defines the target to
/// migrate to. Without this, Migrate will make a From impl for IdentVX to Ident.
#[proc_macro_attribute]
pub fn migrate_to_version(_attr: TokenStream, item: TokenStream) -> TokenStream {
    item
}

/// Adds a From implementation to help migrate structs or enums.  This will support the following
/// migrations:
///
///   1. Adding a new member to a struct where the member has a Default implementation.
///   2. Changing the type of a member with the same name, where all the fields implement From.
///      Default is not needed in this case by including the the migrate_nodefault attribute.
///   3. Changing an enum variant's fields where the all the fields implement From.
///
/// This will also work if a new variant is added to an enum, but if that's the only change, and
/// it's added to the end of the exsting variants, it will automatically be backward compatible
/// because it won't affect how bincode serializes the old variants.  There are some other changes
/// that can be made that are safe with bincode serialization, such as converting from Option to
/// Vec.
#[proc_macro_derive(Migrate)]
pub fn derive_migrate(input: TokenStream) -> TokenStream {
    let input: syn::DeriveInput = parse_macro_input!(input);

    let ident = input.ident;
    let target = if let Some(attr) = input.attrs.iter().find(|a| {
        a.style == syn::AttrStyle::Outer
            && a.path.get_ident().is_some()
            && a.path.get_ident().unwrap().to_string() == "migrate_to_version"
    }) {
        match attr.parse_args::<syn::Type>() {
            Ok(ident) => ident.into_token_stream(),
            Err(error) => error.into_compile_error(),
        }
    } else {
        match format!("{}", ident).rsplit_once('V') {
            Some((ident, _)) => format_ident!("{}", ident).into_token_stream(),
            None => syn::Error::new(
                ident.span(),
                "struct or enum must have V in the name to be migrated.",
            )
            .into_compile_error(),
        }
    };

    let out = match input.data {
        Data::Enum(e) => {
            let mut arms = quote! {};
            for variant in e.variants {
                let var_ident = variant.ident;
                let (fields, result) = match variant.fields {
                    Fields::Named(fields) => {
                        let field_names: Vec<_> = fields.named.iter().map(|f| &f.ident).collect();
                        (
                            quote! { { #(#field_names),* } },
                            quote! {
                                #target::#var_ident { #(#field_names: #field_names.into()),* }
                            },
                        )
                    }
                    Fields::Unnamed(fields) => {
                        let len = fields.unnamed.len();
                        let field_names: Vec<_> =
                            (0..len).map(|i| format_ident!("f{}", i)).collect();
                        (
                            quote! { (#(#field_names),*) },
                            quote! { #target::#var_ident(#(#field_names.into()),*) },
                        )
                    }
                    Fields::Unit => (quote! {}, quote! { #target::#var_ident }),
                };

                arms.extend(quote! {
                    #ident::#var_ident #fields => #result,
                });
            }

            quote! {
                impl From<#ident> for #target {
                    fn from(from: #ident) -> Self {
                        match from {
                            #arms
                        }
                    }
                }
            }
        }
        Data::Struct(s) => {
            let field_names = s.fields.iter().map(|f| &f.ident);

            let default_string = if input
                .attrs
                .iter()
                .filter(|a| {
                    a.style == syn::AttrStyle::Outer
                        && a.path.get_ident().is_some()
                        && a.path.get_ident().unwrap().to_string() == "migrate_nodefault"
                })
                .collect::<Vec<&syn::Attribute>>()
                .is_empty()
            {
                quote! {
                    ..Default::default()
                }
            } else {
                quote! {}
            };

            quote! {
                impl From<#ident> for #target {
                    fn from(from: #ident) -> Self {
                        #target {
                            #(#field_names: from.#field_names.into()),*,
                            #default_string
                        }
                    }
                }
            }
        }
        _ => unimplemented!(),
    };

    // eprintln!("TOKENS: {}", out);

    out.into()
}
