// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use proc_macro2::Span;
use quote::{quote, ToTokens};
use syn::{
    parse::Parse, parse_macro_input, punctuated::Punctuated, spanned::Spanned, Attribute, Generics,
    Ident, Lit, Path, Token, Type, WherePredicate,
};

// Reduces code duplication for these commonly-used token streams.

static CRATE: LazyTokens = LazyTokens::new(|| quote!(::ffx_validation));
static CRATE_SCHEMA: LazyTokens = LazyTokens::new(|| quote!(#CRATE::schema));
static TYPE: LazyTokens = LazyTokens::new(|| quote!(#CRATE_SCHEMA::Type));
static VALUE_TYPE: LazyTokens = LazyTokens::new(|| quote!(#CRATE_SCHEMA::ValueType));
static INLINE_VALUE: LazyTokens = LazyTokens::new(|| quote!(#CRATE_SCHEMA::InlineValue));
static SCHEMA: LazyTokens = LazyTokens::new(|| quote!(#CRATE::Schema));

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
/// * The `#[foreign(RealType)]` attribute is used to create a type alias for a type declared
///   outside of the current crate.
///
///   Example: `struct Point2DM; /* Marker type */`
///   `#[foreign(Point2D)] type Point2DM = struct { x: f32, y: f32 };`
///   `type Polygon = struct { points: [Point2DM], center: Point2DM };`
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

#[proc_macro_derive(Schema)]
pub fn schema_derive(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    // Essentially turn the item into a schema item.
    let mut item = parse_macro_input!(input as syn::DeriveInput);

    // TODO(https://fxbug.dev/320578372): Support where clause attributes: #[schema(where T: Schema + 'static)]

    // TODO: Support for transparent schemas: #[schema(transparent)]
    // Type and lifetime parameters don't need static bounds for a transparent schema.
    let schema_trait_bound: syn::TypeParamBound = syn::parse2(SCHEMA.to_token_stream()).unwrap();
    let static_lifetime = syn::Lifetime::new("'static", Span::call_site());

    // Where predicates for the generated schema impl's where clause.
    let mut clauses = Vec::<WherePredicate>::new();

    // Aliases use std::any::TypeId, which requires the struct to be static.
    // Add static lifetime bounds to all lifetimes appearing in the type.
    for lt in item.generics.lifetimes() {
        // impl<'lt> Schema for MyStruct<'lt> where 'lt: 'static
        //                                          ^^^^^^^^^^^^
        clauses.push(WherePredicate::Lifetime(syn::PredicateLifetime {
            lifetime: lt.lifetime.clone(),
            colon_token: Default::default(),
            bounds: Punctuated::from_iter([static_lifetime.clone()]),
        }));
    }

    // Also add static lifetime bounds to type parameters.
    for ty_param in item.generics.type_params() {
        // impl<T> Schema for MyStruct<T> where T: 'static
        //                                      ^^^^^^^^^^^
        clauses.push(WherePredicate::Type(syn::PredicateType {
            lifetimes: None,
            bounded_ty: Type::Path(syn::TypePath {
                qself: None,
                path: Path::from(ty_param.ident.clone()),
            }),
            colon_token: Default::default(),
            bounds: Punctuated::from_iter([syn::TypeParamBound::Lifetime(static_lifetime.clone())]),
        }));
    }

    let ty = match item.data {
        syn::Data::Struct(struct_item) => {
            schema_from_fields(struct_item.fields, &mut clauses, &schema_trait_bound)
        }
        syn::Data::Enum(enum_item) => SchemaType::Enum {
            variants: enum_item
                .variants
                .into_pairs()
                .map(|p| p.into_tuple())
                .map(|(variant, comma)| {
                    let ty = if let syn::Fields::Unit = variant.fields {
                        None
                    } else {
                        Some(schema_from_fields(variant.fields, &mut clauses, &schema_trait_bound))
                    };
                    syn::punctuated::Pair::new(SchemaEnumVariant { name: variant.ident, ty }, comma)
                })
                .collect(),
        },
        _ => {
            return syn::Error::new(item.ident.span(), "Unions are not supported")
                .into_compile_error()
                .into()
        }
    };

    let out_item = {
        let name = item.ident;
        let (_, ty_args, _) = item.generics.split_for_impl();
        let impl_path = quote!(#name #ty_args);

        item.generics.make_where_clause().predicates.extend(clauses);

        SchemaItem::Impl(SchemaImplItem {
            generics: item.generics,
            impl_path,
            ty,
            attr: ImplItemAttr::default(),
        })
    };

    out_item.build().into()
}

/// Turns struct or enum variant fields into a schema type.
fn schema_from_fields(
    fields: syn::Fields,
    clauses: &mut Vec<WherePredicate>,
    schema_trait_bound: &syn::TypeParamBound,
) -> SchemaType {
    match fields {
        // struct Name; -> null type
        syn::Fields::Unit => SchemaType::Null,
        // struct Name { field: u32 } -> struct { field: u32 }
        syn::Fields::Named(fields) => {
            let mut out_fields = Punctuated::new();
            for (field, comma) in fields.named.pairs().map(|p| p.into_tuple()) {
                out_fields.push_value(SchemaField {
                    key: SchemaStructKey {
                        name: field.ident.clone().unwrap(),
                        // TODO(https://fxbug.dev/320578372): Optional field attribute
                        // #[schema(optional)] or #[serde(optional)]
                        optional: false,
                    },
                    value: SchemaType::Alias(field.ty.clone()),
                });
                if let Some(comma) = comma {
                    out_fields.push_punct(comma.clone());
                }

                // Add schema bound to the where clause.
                // for struct MyStruct { field: Type } -> impl ... where Type: Schema
                //                                                       ^^^^^^^^^^^^
                clauses.push(WherePredicate::Type(syn::PredicateType {
                    lifetimes: None,
                    bounded_ty: field.ty.clone(),
                    colon_token: Default::default(),
                    bounds: Punctuated::from_iter([schema_trait_bound.clone()]),
                }));
            }
            SchemaType::Struct { fields: out_fields }
        }
        // struct Name(u32, String); -> (u32, String)
        syn::Fields::Unnamed(fields) => {
            let mut elems = Punctuated::new();
            for (field, comma) in fields.unnamed.pairs().map(|p| p.into_tuple()) {
                elems.push_value(SchemaType::Alias(field.ty.clone()));
                if let Some(comma) = comma {
                    elems.push_punct(comma.clone());
                }

                // Add schema bound to the where clause.
                // for struct MyStruct { field: Type } -> impl ... where Type: Schema
                //                                                       ^^^^^^^^^^^^
                clauses.push(WherePredicate::Type(syn::PredicateType {
                    lifetimes: None,
                    bounded_ty: field.ty.clone(),
                    colon_token: Default::default(),
                    bounds: Punctuated::from_iter([schema_trait_bound.clone()]),
                }));
            }
            if elems.len() == 1 {
                // Serde newtype struct
                elems.pop().unwrap().into_value()
            } else {
                // Serde tuple struct
                SchemaType::Tuple(elems)
            }
        }
    }
}

fn stringify_ident(id: &Ident) -> String {
    let mut s = id.to_string();
    if s.starts_with("r#") {
        s.drain(..2);
    }
    s
}

/// Parser & container for struct fields & json object elements in the form `K: V`.
struct SchemaField<K, V = K> {
    key: K,
    value: V,
}

struct SchemaStructKey {
    name: Ident,
    optional: bool,
}

struct SchemaEnumVariant {
    // TODO: Support string literals as variant names
    name: Ident,
    ty: Option<SchemaType>,
}

#[derive(Default)]
struct ImplItemAttr {
    transparent: bool,
    foreign: Option<Type>,
    recursive: bool,
}

struct SchemaImplItem {
    generics: Generics,
    impl_path: proc_macro2::TokenStream,
    ty: SchemaType,
    attr: ImplItemAttr,
}

enum SchemaItem {
    Impl(SchemaImplItem),
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
    Null,
    Union(Vec<SchemaType>),
    Alias(Type),
    Literal(SchemaLiteral),
    Struct {
        // TODO(b/316035130): Support string literals as field keys
        fields: Punctuated<SchemaField<SchemaStructKey, SchemaType>, Token![,]>,
        // TODO(b/316035760): Spread syntax (needs SchemaStructField enum)
        // TODO(b/316035686): Struct attributes (for #[strict])
    },
    Enum {
        // TODO: Support serde rename_all?
        variants: Punctuated<SchemaEnumVariant, Token![,]>,
    },
    Optional(Box<SchemaType>),
    // Tuples are not parsed by `SchemaType::parse`.
    // This is used to emit tuples for structs & enums
    Tuple(Punctuated<SchemaType, Token![,]>),
}

impl ImplItemAttr {
    fn duplicate_alias_attr_error(attr: &Attribute) -> syn::Error {
        syn::Error::new(
            attr.span(),
            concat!(
                "Conflicting alias attribute. ",
                "Only one #[transparent] or #[foreign(...)] allowed."
            ),
        )
    }

    fn from_attrs(attrs: &mut Vec<Attribute>) -> syn::Result<Self> {
        let mut out = Self::default();
        let mut errors = Vec::new();
        attrs.retain_mut(|attr| {
            if attr.path.is_ident("transparent") {
                // #[transparent]
                if out.foreign.is_some() {
                    errors.push(Self::duplicate_alias_attr_error(attr));
                }

                out.transparent = true;
                false
            } else if attr.path.is_ident("foreign") {
                // #[foreign(<Type>)]
                if out.transparent {
                    errors.push(Self::duplicate_alias_attr_error(attr));
                }

                let res =
                    attr.parse_args_with(|input: syn::parse::ParseStream<'_>| -> syn::Result<()> {
                        let ty = input.parse()?;
                        input.parse::<syn::parse::Nothing>()?;
                        out.foreign = Some(ty);
                        Ok(())
                    });

                if let Err(err) = res {
                    errors.push(err);
                }

                false
            } else if attr.path.is_ident("recursive") {
                // #[recursive]
                out.recursive = true;

                false
            } else {
                true
            }
        });

        if !errors.is_empty() {
            // Combine all errors into one single error.
            let mut iter = errors.drain(..);
            let mut first = iter.next().unwrap();
            first.extend(iter);
            return Err(first);
        }

        Ok(out)
    }
}

// Parses:
// `Attribute...` type Type<T> where T: Trait = `SchemaType`;
// `Attribute...` impl<T> for module::ImplPath<T> where T: Trait = `SchemaType`;
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
        } else {
            return Err(lookahead.error());
        })
    }
}

fn maybe_wrap_alias(
    ty: Option<impl quote::ToTokens>,
    schema_ty: &SchemaType,
    attr: &ImplItemAttr,
) -> proc_macro2::TokenStream {
    if let Some(ty) = ty {
        let mut body = schema_ty.build();
        if attr.recursive {
            body = quote! {
                #CRATE_SCHEMA::RecursiveType::Fn(|| #body)
            };
        } else {
            body = quote! {
                #CRATE_SCHEMA::RecursiveType::Plain(#body)
            };
        }
        quote! {
            &#TYPE::Alias {
                name: ::std::any::type_name::<#ty>,
                id: ::std::any::TypeId::of::<#ty>,
                ty: #body
            }
        }
    } else {
        schema_ty.build()
    }
}

impl SchemaImplItem {
    fn build(&self) -> proc_macro2::TokenStream {
        let SchemaImplItem { generics, impl_path, ty, attr } = self;
        let (impl_generics, _, where_clause) = generics.split_for_impl();

        let body = maybe_wrap_alias(
            if attr.transparent {
                None
            } else {
                Some(
                    attr.foreign
                        .as_ref()
                        .map(ToTokens::into_token_stream)
                        .unwrap_or(quote! { Self }),
                )
            },
            ty,
            attr,
        );

        quote! {
            impl #impl_generics #SCHEMA for #impl_path #where_clause {
                const TYPE: &'static #TYPE<'static> = #body;
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

// Parses:
// identifier
// identifier?
impl Parse for SchemaStructKey {
    fn parse(input: syn::parse::ParseStream<'_>) -> syn::Result<Self> {
        Ok(Self { name: input.parse()?, optional: input.parse::<Option<Token![?]>>()?.is_some() })
    }
}

// Parses:
// Variant
// Variant( `SchemaType,...` )
// Variant { `field: SchemaType,...` }
impl Parse for SchemaEnumVariant {
    fn parse(input: syn::parse::ParseStream<'_>) -> syn::Result<Self> {
        let name = input.parse()?;

        let lookahead = input.lookahead1();
        let mut ty = None;

        // TODO: Support = for constants?
        if lookahead.peek(syn::token::Paren) {
            // Tuple enum variant
            let paren;
            syn::parenthesized!(paren in input);
            let paren = paren.parse_terminated::<_, Token![,]>(SchemaType::parse)?;
            if paren.len() == 1 {
                ty = paren.into_iter().next();
            } else {
                ty = Some(SchemaType::Tuple(paren));
            }
        } else if lookahead.peek(syn::token::Brace) {
            // Struct enum variant
            let braced;
            syn::braced!(braced in input);
            ty = Some(SchemaType::Struct { fields: braced.parse_terminated(SchemaField::parse)? });
        } else {
            // Empty enum variant
        }

        Ok(SchemaEnumVariant { name, ty })
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
// enum { `SchemaEnumVariant,...` }
// const `SchemaLiteral`
// `Type`
impl Parse for SchemaType {
    fn parse(input: syn::parse::ParseStream<'_>) -> syn::Result<Self> {
        let lookahead = input.lookahead1();

        let plain_literal = lookahead.peek(syn::LitStr)
            || lookahead.peek(syn::LitBool)
            || lookahead.peek(syn::LitInt)
            || lookahead.peek(syn::LitFloat);

        let mut ty = if lookahead.peek(Token![struct]) {
            input.parse::<Token![struct]>()?;
            let braced;
            syn::braced!(braced in input);
            Self::Struct { fields: braced.parse_terminated(SchemaField::parse)? }
        } else if lookahead.peek(Token![enum]) {
            input.parse::<Token![enum]>()?;
            let braced;
            syn::braced!(braced in input);
            Self::Enum { variants: braced.parse_terminated(SchemaEnumVariant::parse)? }
        } else if lookahead.peek(Token![const]) || plain_literal {
            if !plain_literal {
                input.parse::<Token![const]>()?;
            }
            Self::Literal(input.parse()?)
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
            let mut tys = vec![ty];
            loop {
                let ty = input.parse()?;
                if let SchemaType::Union(types) = ty {
                    tys.extend(types);
                } else {
                    tys.push(ty);
                }

                if input.parse::<Option<Token![|]>>()?.is_none() {
                    break;
                }
            }
            ty = Self::Union(tys);
        }

        Ok(ty)
    }
}

impl SchemaType {
    fn build(&self) -> proc_macro2::TokenStream {
        match self {
            Self::Null => quote! {
                &#TYPE::Type { ty: #VALUE_TYPE::Null }
            },
            Self::Union(tys) => {
                let tys = tys.iter().map(Self::build);
                quote! {
                    &#TYPE::Union(&[#(#tys),*])
                }
            }
            Self::Alias(ty) => {
                quote! {
                    <#ty as #SCHEMA>::TYPE
                }
            }
            Self::Literal(lit) => {
                let constant = match lit {
                    // TODO(b/316036318): support array and object literals
                    SchemaLiteral::Simple(lit) => match lit {
                        Lit::Str(lit) => quote! { #INLINE_VALUE::String(#lit) },
                        Lit::Int(lit) => {
                            if lit.base10_parse::<u64>().is_ok() {
                                quote! { #INLINE_VALUE::UInt(#lit as u64) }
                            } else {
                                quote! { #INLINE_VALUE::Int(#lit as i64) }
                            }
                        }
                        Lit::Float(lit) => quote! { #INLINE_VALUE::Float(#lit as f64) },
                        Lit::Bool(lit) => quote! { #INLINE_VALUE::Bool(#lit) },
                        _ => unreachable!("Literals should be vetted by SchemaLiteral::parse"),
                    },
                    _ => todo!("literals not yet supported"),
                };

                quote! {
                    &#TYPE::Constant { value: &#constant }
                }
            }
            Self::Struct { fields } => {
                let fields: proc_macro2::TokenStream = fields
                    .iter()
                    .map(|field| {
                        let SchemaField { key, value } = field;
                        let SchemaStructKey { name, optional } = key;
                        let ty = value.build();
                        let key_str = stringify_ident(name);
                        quote! {
                            #CRATE_SCHEMA::Field {
                                key: #key_str,
                                value: #ty,
                                optional: #optional,
                                ..#CRATE_SCHEMA::FIELD
                            },
                        }
                    })
                    .collect();

                quote! {
                    &#TYPE::Struct {
                        fields: &[#fields],
                        extras: None,
                    }
                }
            }
            Self::Enum { variants } => {
                let variants: proc_macro2::TokenStream = variants
                    .iter()
                    .map(|variant| {
                        let key_str = stringify_ident(&variant.name);
                        let ty = match &variant.ty {
                            None => quote! { &#TYPE::Void },
                            Some(ty) => ty.build(),
                        };
                        quote! {
                            (#key_str, #ty),
                        }
                    })
                    .collect();
                quote! {
                    &#TYPE::Enum {
                        variants: &[#variants],
                    }
                }
            }
            Self::Optional(ty) => {
                let ty = ty.build();
                quote! { &#TYPE::Union(&[#ty, &#TYPE::Type { ty: #VALUE_TYPE::Null }]) }
            }
            Self::Tuple(tys) => {
                let tys: proc_macro2::TokenStream = tys
                    .iter()
                    .map(|ty| {
                        let ty = ty.build();
                        quote! { #ty, }
                    })
                    .collect();
                quote! { &#TYPE::Tuple { fields: &[#tys] } }
            }
        }
    }
}

/// Procedural macro token streams are not const & thread safe so they
/// cannot be used within a static.
struct LazyTokens(fn() -> proc_macro2::TokenStream);

impl LazyTokens {
    const fn new(init: fn() -> proc_macro2::TokenStream) -> Self {
        Self(init)
    }
}

/// Allows the [`quote::quote!`] macro to interpolate these directly.
impl quote::ToTokens for LazyTokens {
    fn to_tokens(&self, tokens: &mut proc_macro2::TokenStream) {
        (self.0)().to_tokens(tokens)
    }

    fn to_token_stream(&self) -> proc_macro2::TokenStream {
        (self.0)()
    }
}
