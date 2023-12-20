// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use quote::quote;
use syn::{
    parse::Parse, parse_macro_input, punctuated::Punctuated, Generics, Ident, Lit, Path, Token,
    Type,
};

// TODO(b/316034512): Derive macro for structs and enums

/// A macro to declare a schema via Rust-like schema syntax.
///
/// # Language
///
/// At a high-level, schemas use a modified version of Rust's syntax. Here's a gist of the changes:
///
/// ## Items
///
/// * `type` aliases apply an impl to a type within scope, instead of introducing a named type.
///
/// * `impl`s omit the trait, since the trait is always known.
///
///   Example: `impl for serde_json::Value = json::Any`
///
/// ## Types
///
/// * Types can be represented as a union of multiple subtypes.
///
///   Example: `TypeA | TypeB | TypeC`
///
/// * Optional types can use a `?` suffix as a shorthand for `Option<T>`.
///
///   Example: `type Option<T: Schema> = T?`
///
/// * Anonymous struct and enums can be used as types.
///
///   Example: `struct { field_a: bool, field_b: u32 }`
///   `enum { One, Two, Three }`
///
///     * Struct fields can be marked optional (for presence).
///
///       Example: `field_a?: bool`
///
///     * Struct fields can inline struct and map fields from other types.
///
///       Example: `struct { field_c: f64, ..ExampleStruct }`
///
///     * Structs can be marked as strict, disallowing unknown fields.
///
///       Example: `#[strict] struct { single: u32 }`
///
///     * Struct fields and enum variants can use string constants as names.
///
///       Example: `struct { "field-d": ABC }`
///       `enum Status { "incomplete-data", "ok", "unknown-error"(String) }`
///
/// * JSON constants can be used as types, but require a `const` prefix as a disambiguator.
///
///   Example: `const "hello-world"`
///   `const [1, 2, 3]`
///
/// * Functions can be used as types using the `fn` prefix.
///
///   Example: `fn my_lone_function`
///
/// # Serde Compatibility
///
/// By default, enums are compatible with _simple_ Serde enums. Serde attributes that heavily
/// change the structure of an enum (e.g. tag fields) must be represented as a union of structs
/// instead.
#[proc_macro]
pub fn schema(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    let items = parse_macro_input!(input as SchemaItems);

    items.items.iter().map(|item| item.build()).collect::<proc_macro2::TokenStream>().into()
}

struct SchemaField<K, V = K> {
    key: K,
    value: V,
}

struct StructItemImpl {
    // TODO: #[transparent] attribute to remove add_alias
    generics: Generics,
    name: Ident,
    impl_path: proc_macro2::TokenStream,
    ty: SchemaType,
}

enum SchemaItem {
    Impl(StructItemImpl),
}

struct SchemaItems {
    items: Punctuated<SchemaItem, Token![;]>,
}

enum SchemaLiteral {
    Simple(Lit),
    Map(Punctuated<SchemaField<SchemaLiteral>, Token![,]>),
    Array(Punctuated<SchemaLiteral, Token![,]>),
}

enum SchemaType {
    Union(Vec<SchemaType>),
    Alias(Type),
    Literal(SchemaLiteral),
    Struct {
        // TODO(b/316035130): Support string literals as field keys
        fields: Punctuated<SchemaField<syn::Ident, SchemaType>, Token![,]>,
        // TODO(b/316035760): Spread syntax (needs SchemaStructField enum)
        // TODO(b/316035686): Struct attributes (for #[strict])
    },
    // TODO: enums
    // TODO: fn <path> to indicate a direct call to a walker function
    Optional(Box<SchemaType>),
}

// Parses:
// type Type<T> where T: Trait = `SchemaType`;
// impl<T> for module::ImplPath<T> where T: Trait = `SchemaType`;
impl Parse for SchemaItem {
    fn parse(input: syn::parse::ParseStream<'_>) -> syn::Result<Self> {
        let lookahead = input.lookahead1();
        Ok(if lookahead.peek(Token![type]) {
            // type Type<T> where T: Trait = T...;

            input.parse::<Token![type]>()?;

            let name = input.parse()?;

            let mut generics = if input.lookahead1().peek(Token![<]) {
                input.parse::<Generics>()?
            } else {
                Default::default()
            };

            if input.lookahead1().peek(Token![where]) {
                *generics.make_where_clause() = input.parse()?;
            }

            let (_, ty_args, _) = generics.split_for_impl();

            let impl_path = quote!(#name #ty_args);

            input.parse::<Token![=]>()?;

            let ty = input.parse()?;

            Self::Impl(StructItemImpl { generics, name, impl_path, ty })
        } else if lookahead.peek(Token![impl]) {
            // impl<T> for module::ImplPath<T> where T: Trait = T...;

            input.parse::<Token![impl]>()?;

            let mut generics = {
                // `<T>`? for
                let lookahead = input.lookahead1();

                if input.lookahead1().peek(Token![<]) {
                    input.parse::<Generics>().map_err(|mut err| {
                        err.combine(lookahead.error());
                        err
                    })?
                } else {
                    Default::default()
                }
            };

            input.parse::<Token![for]>()?;

            let path = input.parse::<Path>()?;

            if input.lookahead1().peek(Token![where]) {
                *generics.make_where_clause() = input.parse()?;
            }

            input.parse::<Token![=]>()?;
            let ty = input.parse()?;

            Self::Impl(StructItemImpl {
                generics,
                name: path.segments.last().unwrap().ident.clone(),
                impl_path: quote!(#path),
                ty,
            })
        } else if lookahead.peek(Token![enum]) {
            return Err(input.error("inline enums not yet supported"));
        } else if lookahead.peek(Token![fn]) {
            return Err(input.error("fn references not yet supported"));
        } else {
            return Err(lookahead.error());
        })
    }
}

impl StructItemImpl {
    fn build(&self) -> proc_macro2::TokenStream {
        let StructItemImpl { generics, name, impl_path, ty } = self;
        let name_str = name.to_string();
        let walker = quote! { walker };
        let body = ty.build(&walker);
        let (impl_generics, _, where_clause) = generics.split_for_impl();
        quote! {
            impl #impl_generics ::ffx_validation::schema::Schema for #impl_path #where_clause {
                fn walk_schema(#walker: &mut dyn ::ffx_validation::schema::Walker) -> ::std::ops::ControlFlow<()> {
                    // TODO: Use std::any::type_name
                    #walker
                        .add_alias(#name_str, ::std::any::TypeId::of::<Self>(), |#walker| {
                            #body
                            #walker.ok()
                        })?
                        .ok()
                }
            }
        }
    }
}

impl SchemaItem {
    fn build(&self) -> proc_macro2::TokenStream {
        match self {
            SchemaItem::Impl(item) => item.build(),
        }
    }
}

// Parses semicolon-separated items.
// If a single item was parsed a trailing semicolon is not required.
impl Parse for SchemaItems {
    fn parse(input: syn::parse::ParseStream<'_>) -> syn::Result<Self> {
        let items = input.parse_terminated(SchemaItem::parse)?;
        if items.len() > 1 && !items.trailing_punct() {
            input.parse::<Token![;]>()?;
        }
        Ok(Self { items })
    }
}

// Parses a JSON struct, array, or Rust constant literal.
impl Parse for SchemaLiteral {
    fn parse(input: syn::parse::ParseStream<'_>) -> syn::Result<Self> {
        let lookahead = input.lookahead1();
        Ok(if lookahead.peek(syn::token::Brace) {
            let braced;
            syn::braced!(braced in input);
            Self::Map(braced.parse_terminated(SchemaField::parse)?)
        } else if lookahead.peek(syn::token::Bracket) {
            let bracketed;
            syn::bracketed!(bracketed in input);
            Self::Array(bracketed.parse_terminated(SchemaLiteral::parse)?)
        } else {
            Self::Simple(match input.parse::<Lit>() {
                Ok(t) => t,
                Err(mut e) => {
                    e.combine(lookahead.error());
                    return Err(e);
                }
            })
        })
    }
}

// Parses `K`: `V`
impl<K: Parse, V: Parse> Parse for SchemaField<K, V> {
    fn parse(input: syn::parse::ParseStream<'_>) -> syn::Result<Self> {
        let key = input.parse()?;
        input.parse::<Token![:]>()?;
        let value = input.parse()?;
        Ok(Self { key, value })
    }
}

// Parses:
// struct { field: `Type`, ... }
// const `SchemaLiteral`
// `Type`
impl Parse for SchemaType {
    fn parse(input: syn::parse::ParseStream<'_>) -> syn::Result<Self> {
        let mut ty = if input.parse::<Option<Token![struct]>>()?.is_some() {
            let braced;
            syn::braced!(braced in input);
            Self::Struct { fields: braced.parse_terminated(SchemaField::parse)? }
        } else if input.parse::<Option<Token![const]>>()?.is_some() {
            Self::Literal(input.parse::<SchemaLiteral>()?)
        } else if let Ok(ty) = input.parse::<Type>() {
            Self::Alias(ty)
        } else {
            return Err(input.error("Expected type or literal"));
        };

        if let Some(..) = input.parse::<Option<Token![?]>>()? {
            ty = Self::Optional(Box::new(ty));
        }

        if let Some(..) = input.parse::<Option<Token![|]>>()? {
            let mut tys = vec![ty, input.parse()?];
            while let Some(..) = input.parse::<Option<Token![|]>>()? {
                tys.push(input.parse()?);
            }
            ty = Self::Union(tys);
        }

        Ok(ty)
    }
}

impl SchemaType {
    fn build(&self, walker: &proc_macro2::TokenStream) -> proc_macro2::TokenStream {
        match self {
            Self::Union(tys) => tys.iter().map(|ty| ty.build(walker)).collect(),
            Self::Alias(ty) => {
                quote! {
                    <#ty as ::ffx_validation::schema::Schema>::walk_schema(#walker)?;
                }
            }
            Self::Literal(_lit) => {
                // TODO(b/316036318): serde_json macro invocation
                todo!("literals not yet supported")
            }
            Self::Struct { fields } => {
                let fields: proc_macro2::TokenStream = fields.iter().map(|field| {
                    let ty = field.value.build(walker);
                    let key_str = field.key.to_string();
                    quote! {
                        ::ffx_validation::schema::Field { key: #key_str, value: |#walker| { #ty walker.ok() }, ..::ffx_validation::schema::FIELD },
                    }
                }).collect();
                quote! {
                    #walker.add_struct(
                        &[#fields],
                        None
                    )?;
                }
            }
            Self::Optional(ty) => {
                let ty = ty.build(walker);
                quote! {
                    #ty
                    #walker.add_type(::ffx_validation::schema::ValueType::Null)?;
                }
            }
        }
    }
}
