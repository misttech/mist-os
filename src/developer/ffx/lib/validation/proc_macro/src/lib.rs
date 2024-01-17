// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use quote::quote;
use syn::{
    parse::Parse, parse_macro_input, punctuated::Punctuated, Attribute, Generics, Ident, Lit, Path,
    Token, Type,
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
/// * `type` and `impl` can use the `#[transparent]` attribute to emit the raw schema type instead
///   of wrapping it in a type alias.
///
/// * `fn` declares a plain `[ffx_validation::schema::Walker]` function without an internal type
///   alias.
///
///   Example: `fn reusable_schema = u32 | bool;`
///
/// * `fn` can use the `#[foreign(RealType)]` attribute to create a type alias for a type declared
///   outside of the current crate.
///
///   Example: `#[foreign(Point2D)] fn point2d = struct { x: f32, y: f32 };`
///   `type Polygon = struct { points: [fn point2d], center: fn point2d };`
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

#[derive(Default)]
struct ImplItemAttr {
    transparent: bool,
}

struct SchemaImplItem {
    generics: Generics,
    impl_path: proc_macro2::TokenStream,
    ty: SchemaType,
    attr: ImplItemAttr,
}

#[derive(Default)]
struct FnItemAttr {
    foreign: Option<Type>,
}

struct SchemaFnItem {
    name: Ident,
    generics: Generics,
    ty: SchemaType,
    attr: FnItemAttr,
}

enum SchemaItem {
    Impl(SchemaImplItem),
    Fn(SchemaFnItem),
}

struct SchemaItems {
    items: Punctuated<SchemaItem, Token![;]>,
}

#[allow(dead_code)] // TODO(https://fxbug.dev/318827209)
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
    Fn(Path),
    Optional(Box<SchemaType>),
}

impl ImplItemAttr {
    fn from_attrs(attrs: &mut Vec<Attribute>) -> syn::Result<Self> {
        let mut out = Self::default();
        attrs.retain_mut(|attr| {
            if attr.path.is_ident("transparent") {
                out.transparent = true;
                false
            } else {
                true
            }
        });
        Ok(out)
    }
}

impl FnItemAttr {
    fn from_attrs(attrs: &mut Vec<Attribute>) -> syn::Result<Self> {
        let mut out = Self::default();
        let mut errors = Vec::new();
        attrs.retain_mut(|attr| {
            if attr.path.is_ident("foreign") {
                let res =
                    attr.parse_args_with(|input: syn::parse::ParseStream<'_>| -> syn::Result<()> {
                        let paren;
                        syn::parenthesized!(paren in input);
                        let ty = paren.parse()?;
                        paren.parse::<syn::parse::Nothing>()?;
                        out.foreign = Some(ty);
                        Ok(())
                    });

                if let Err(err) = res {
                    errors.push(err);
                }

                false
            } else {
                true
            }
        });
        Ok(out)
    }
}

// Parses:
// `Attribute...` type Type<T> where T: Trait = `SchemaType`;
// `Attribute...` impl<T> for module::ImplPath<T> where T: Trait = `SchemaType`;
// `Attribute...` fn module::fn_path = `SchemaType`;
impl Parse for SchemaItem {
    fn parse(input: syn::parse::ParseStream<'_>) -> syn::Result<Self> {
        let mut attrs = Vec::new();

        if input.peek(Token![#]) {
            attrs = Attribute::parse_outer(input)?;
        }

        let lookahead = input.lookahead1();

        Ok(if lookahead.peek(Token![type]) {
            // type Type<T> where T: Trait = T...;

            input.parse::<Token![type]>()?;

            let name = input.parse::<Ident>()?;

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

            Self::Impl(SchemaImplItem {
                generics,
                impl_path,
                ty,
                attr: ImplItemAttr::from_attrs(&mut attrs)?,
            })
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

            Self::Impl(SchemaImplItem {
                generics,
                impl_path: quote!(#path),
                ty,
                attr: ImplItemAttr::from_attrs(&mut attrs)?,
            })
        } else if lookahead.peek(Token![enum]) {
            return Err(input.error("inline enums not yet supported"));
        } else if lookahead.peek(Token![fn]) {
            input.parse::<Token![fn]>()?;

            let name = input.parse()?;
            let generics = if input.lookahead1().peek(Token![<]) {
                input.parse::<Generics>()?
            } else {
                Default::default()
            };

            input.parse::<Token![=]>()?;
            let ty = input.parse()?;

            Self::Fn(SchemaFnItem { name, generics, ty, attr: FnItemAttr::from_attrs(&mut attrs)? })
        } else {
            return Err(lookahead.error());
        })
    }
}

fn maybe_wrap_alias(
    ty: Option<impl quote::ToTokens>,
    walker: &proc_macro2::TokenStream,
    body: proc_macro2::TokenStream,
) -> proc_macro2::TokenStream {
    if let Some(ty) = ty {
        quote! {
            #walker
                .add_alias(
                    ::std::any::type_name::<#ty>(),
                    ::std::any::TypeId::of::<#ty>(),
                    |#walker| {
                        #body
                        #walker.ok()
                    }
                )?;
        }
    } else {
        body
    }
}

impl SchemaImplItem {
    fn build(&self) -> proc_macro2::TokenStream {
        let SchemaImplItem { generics, impl_path, ty, attr } = self;
        let walker = quote! { walker };
        let body = ty.build(&walker);
        let (impl_generics, _, where_clause) = generics.split_for_impl();

        let fn_body = maybe_wrap_alias(
            if attr.transparent { None } else { Some(<Token![Self]>::default()) },
            &walker,
            body,
        );

        quote! {
            impl #impl_generics ::ffx_validation::schema::Schema for #impl_path #where_clause {
                fn walk_schema(#walker: &mut dyn ::ffx_validation::schema::Walker) -> ::std::ops::ControlFlow<()> {
                    #fn_body
                    #walker.ok()
                }
            }
        }
    }
}

impl SchemaFnItem {
    fn build(&self) -> proc_macro2::TokenStream {
        let SchemaFnItem { name, generics, ty, attr } = self;
        let walker = quote! { walker };
        let (impl_generics, _, where_clause) = generics.split_for_impl();
        let body = ty.build(&walker);

        let fn_body = maybe_wrap_alias(attr.foreign.as_ref(), &walker, body);

        quote! {
            fn #name #impl_generics (#walker: &mut dyn ::ffx_validation::schema::Walker) -> ::std::ops::ControlFlow<()> #where_clause {
                #fn_body
                #walker.ok()
            }
        }
    }
}

impl SchemaItem {
    fn build(&self) -> proc_macro2::TokenStream {
        match self {
            SchemaItem::Impl(item) => item.build(),
            SchemaItem::Fn(item) => item.build(),
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
        let lookahead = input.lookahead1();
        let mut ty = if lookahead.peek(Token![struct]) {
            input.parse::<Token![struct]>()?;
            let braced;
            syn::braced!(braced in input);
            Self::Struct { fields: braced.parse_terminated(SchemaField::parse)? }
        } else if lookahead.peek(Token![enum]) {
            input.parse::<Token![enum]>()?;
            return Err(input.error("not yet implemented"));
        } else if lookahead.peek(Token![const]) {
            input.parse::<Token![const]>()?;
            Self::Literal(input.parse()?)
        } else if lookahead.peek(Token![fn]) {
            input.parse::<Token![fn]>()?;
            Self::Fn(input.parse()?)
        } else if let Ok(ty) = input.parse::<Type>() {
            Self::Alias(ty)
        } else {
            let mut err = lookahead.error();
            err.combine(input.error("expected type"));
            return Err(err);
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
            Self::Fn(path) => {
                quote! {
                    #path(#walker)?;
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
