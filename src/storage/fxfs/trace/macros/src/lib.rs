// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    proc_macro::TokenStream,
    proc_macro2::TokenStream as TokenStream2,
    quote::quote,
    std::vec::Vec,
    syn::{
        parse::{Parse, ParseStream},
        parse_macro_input, parse_quote,
        spanned::Spanned,
        visit_mut::VisitMut,
        Attribute, Block, Ident, ImplItem, ItemFn, ItemImpl, LitStr, ReturnType, Token, Type,
    },
};

enum TraceItem {
    Impl(ItemImpl),
    Fn(ItemFn),
}

impl Parse for TraceItem {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        // Determine if the attribute is on a `fn` or an `impl`. Both a `fn` and an `impl`` can have
        // other attributes and `unsafe` out front. Those tokens are consumed and put back later. An
        // `impl` block should only have the `impl` token next.
        let attrs = input.call(Attribute::parse_outer)?;
        let unsafe_token =
            if input.peek(Token![unsafe]) { Some(input.parse::<Token![unsafe]>()?) } else { None };
        let trace_item = if input.peek(Token![impl]) {
            let mut item_impl = input.parse::<ItemImpl>()?;
            item_impl.attrs = attrs;
            item_impl.unsafety = unsafe_token;
            TraceItem::Impl(item_impl)
        } else {
            let mut item_fn = input.parse::<ItemFn>()?;
            item_fn.attrs = attrs;
            item_fn.sig.unsafety = unsafe_token;
            TraceItem::Fn(item_fn)
        };
        Ok(trace_item)
    }
}

struct TraceImplArgs {
    trace_all_methods: bool,
    prefix: Option<String>,
}

impl Parse for TraceImplArgs {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let mut args = Self { trace_all_methods: false, prefix: None };
        loop {
            if input.is_empty() {
                break;
            }
            let ident: Ident = input.parse()?;
            match ident.to_string().as_ref() {
                "trace_all_methods" => args.trace_all_methods = true,
                "prefix" => {
                    input.parse::<Token![=]>()?;
                    let name: LitStr = input.parse()?;
                    args.prefix = Some(name.value());
                }
                arg => {
                    return Err(syn::Error::new(ident.span(), format!("unknown argument: {}", arg)))
                }
            }
            if input.is_empty() {
                break;
            }
            input.parse::<Token![,]>()?;
        }
        Ok(args)
    }
}

struct TraceMethodArgs {
    name: Option<String>,
}

impl Parse for TraceMethodArgs {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let mut args = Self { name: None };
        if input.is_empty() {
            return Ok(args);
        }
        let content;
        syn::parenthesized!(content in input);
        let ident: Ident = content.parse()?;
        if ident.to_string() != "name" {
            return Err(syn::Error::new(ident.span(), format!("unknown argument: {}", ident)));
        }
        content.parse::<Token![=]>()?;
        let name: LitStr = content.parse()?;
        args.name = Some(name.value());
        if !content.is_empty() {
            return Err(syn::Error::new(content.span(), format!("Unexpected input")));
        }
        if !input.is_empty() {
            return Err(syn::Error::new(input.span(), format!("Unexpected input")));
        }
        Ok(args)
    }
}

struct TraceFnArgs {
    name: Option<String>,
}

impl Parse for TraceFnArgs {
    fn parse(input: ParseStream<'_>) -> syn::Result<Self> {
        let mut args = Self { name: None };
        if input.is_empty() {
            return Ok(args);
        }
        let ident: Ident = input.parse()?;
        if ident.to_string() != "name" {
            return Err(syn::Error::new(ident.span(), format!("unknown argument: {}", ident)));
        }
        input.parse::<Token![=]>()?;
        let name: LitStr = input.parse()?;
        args.name = Some(name.value());
        if !input.is_empty() {
            return Err(syn::Error::new(input.span(), format!("Too many arguments")));
        }
        Ok(args)
    }
}

/// Looks for a `#[trace]` attribute in `attrs`, removes the attribute, and parses the arguments.
/// Returns `Ok(Some(TraceMethodArgs))` if the attribute is present, `Ok(None)` if the attribute is
/// not present, and `Err` if parsing the attribute's arguments fails.
fn remove_trace_attribute(attrs: &mut Vec<Attribute>) -> syn::Result<Option<TraceMethodArgs>> {
    let position = attrs.iter().position(|attr| attr.path.is_ident("trace"));
    let tokens = match position {
        None => {
            return Ok(None);
        }
        Some(pos) => attrs.remove(pos).tokens,
    };
    Ok(Some(syn::parse2(tokens)?))
}

/// Replaces `impl Trait` with `_` in `Type` objects.
struct RemoveImplTrait;
impl VisitMut for RemoveImplTrait {
    fn visit_type_mut(&mut self, node: &mut Type) {
        if let Type::ImplTrait(..) = node {
            *node = Type::Infer(syn::TypeInfer { underscore_token: Token![_](node.span()) });
        } else {
            syn::visit_mut::visit_type_mut(self, node);
        }
    }
}

fn add_tracing_to_async_block(return_type: &ReturnType, block: &mut Block, name: &str) {
    let return_type = if let ReturnType::Type(_, return_type) = return_type {
        let mut return_type = *return_type.clone();
        // `impl Trait` can't appear in the type of a variable binding. Replace all `impl Trait`s
        // with `_`.
        RemoveImplTrait.visit_type_mut(&mut return_type);
        quote!( #return_type )
    } else {
        quote!(())
    };

    // Rust uses the type of the first `return` statement to determine the output type of the future
    // created by the `async move {}' block. If the function returns `Box<dyn Trait>` and has
    // multiple `return` points with different implementations of `Trait` then the generated future
    // will have the type of the first `return` and compilation may fail on the other `return`
    // statements. Placing an unreachable `return` statement with the correct `return` type at the
    // top of the `async move {}` block gives the generated future the correct return type.
    let type_inference_fix = quote! {
        #[allow(unreachable_code)]
        if false {
            let _type_inference_fix: #return_type = unreachable!();
            return _type_inference_fix;
        }
    };

    let stmts = &block.stmts;
    block.stmts = parse_quote!(
        ::fxfs_trace::FxfsTraceFutureExt::trace(async move {
            #type_inference_fix
            #(#stmts)*
        }, ::fxfs_trace::cstr!(#name)).await
    );
}

fn add_tracing_to_sync_block(block: &mut Block, name: &str) {
    let stmts = &block.stmts;
    block.stmts = parse_quote!(
        ::fxfs_trace::duration!(#name);
        #(#stmts)*
    );
}

fn trace_prefix_from_type(impl_type: &Type) -> Option<String> {
    if let Type::Path(path) = impl_type {
        if let Some(segment) = path.path.segments.last() {
            return Some(segment.ident.to_string());
        }
    }
    None
}

fn add_tracing_to_impl(mut item_impl: ItemImpl, args: TraceImplArgs) -> syn::Result<TokenStream2> {
    let prefix = if let Some(prefix) = args.prefix {
        prefix
    } else if let Some(prefix) = trace_prefix_from_type(&*item_impl.self_ty) {
        prefix
    } else {
        return Err(syn::Error::new(
            item_impl.self_ty.span(),
            "Failed to determine prefix from type name. Explicit prefix required.",
        ));
    };

    for item in &mut item_impl.items {
        if let ImplItem::Method(method) = item {
            let trace_fn_args = remove_trace_attribute(&mut method.attrs)?.or_else(|| {
                if args.trace_all_methods {
                    Some(TraceMethodArgs { name: None })
                } else {
                    None
                }
            });
            let Some(trace_fn_args) = trace_fn_args else {
                continue;
            };
            let trace_name = if let Some(name) = trace_fn_args.name {
                name
            } else {
                format!("{}::{}", prefix, method.sig.ident)
            };
            if method.sig.asyncness.is_some() {
                add_tracing_to_async_block(&method.sig.output, &mut method.block, &trace_name);
            } else {
                add_tracing_to_sync_block(&mut method.block, &trace_name);
            }
        }
    }
    Ok(quote!(#item_impl))
}

fn add_tracing_to_fn(mut item_fn: ItemFn, args: TraceFnArgs) -> syn::Result<TokenStream2> {
    let trace_name = if let Some(name) = args.name { name } else { item_fn.sig.ident.to_string() };
    if item_fn.sig.asyncness.is_some() {
        add_tracing_to_async_block(&item_fn.sig.output, &mut item_fn.block, &trace_name);
    } else {
        add_tracing_to_sync_block(&mut item_fn.block, &trace_name);
    }
    Ok(quote!(#item_fn))
}

/// Adds tracing to functions and methods.
///
/// ## Method Tracing
/// When this attribute is present on an `impl`, methods marked with `#[trace]` will have tracing
/// added to them. The name of the trace events default to `<type-name>::<method-name>` but this can
/// be changed with arguments.
///
/// Arguments:
///  - `trace_all_methods` - boolean toggle for whether to add tracing to all methods in the `impl`
///                          even if methods aren't marked with `#[trace]`. Defaults to false.
///  - `prefix`            - string to appear before the method name in the trace events. Defaults
///                          to the name of the type. This argument may be required if the name of
///                          the type can't be determined.
///
/// `#[trace]` Arguments:
///  - `name` - string to use for the name of the trace event. Defaults to
///             `<prefix/type-name>::<method-name>`.
///
/// Example:
/// ```
/// struct Foo;
/// #[fxfs_trace::trace]
/// impl Foo {
///     #[trace]
///     async fn bar(&self) {
///         ...
///     }
/// }
/// ```
///
/// If `async_trait` is also present on the `impl` then `fxfs_trace::trace` should come first.
/// `async_trait` desugars async methods into sync methods that return a BoxFuture. If `fxfs_trace`
/// runs after `async_trait` then `fxfs_trace` will see async methods as sync methods and apply the
/// wrong tracing.
///
/// ## Function Tracing
/// When this attribute is present on a function, the function will have tracing added to it.
///
/// Arguments:
///  - `name` - string to use for the name of the trace event. Defaults to the name of the function.
///
/// ## Synchronous Functions/Methods
/// `fxfs_trace::duration!` is added to the start of the function/method.
///
/// Example:
/// ```
/// #[fxfs_trace::trace]
/// fn example_function() {
///     ...
/// }
/// // Expands to:
/// fn example_function() {
///     fxfs_trace::duration!("example_function");
///     ...
/// }
/// ```
///
/// ## Async Functions/Methods
/// The body of function/method is wrapped in a `fuchsia_tace::TraceFuture`.
///
/// Example:
/// ```
/// #[fxfs_trace::trace]
/// async fn example_function() {
///     ...
/// }
/// // Expands to:
/// async fn example_function() {
///     fxfs_trace::FxfsTraceFutureExt::trace(async move {
///         ...
///     }, fxfs_trace::cstr!("example_function")).await
/// }
/// ```
#[proc_macro_attribute]
pub fn trace(args: TokenStream, input: TokenStream) -> TokenStream {
    let trace_item = parse_macro_input!(input as TraceItem);
    let result = match trace_item {
        TraceItem::Impl(item_impl) => {
            let args = parse_macro_input!(args as TraceImplArgs);
            add_tracing_to_impl(item_impl, args)
        }
        TraceItem::Fn(item_fn) => {
            let args = parse_macro_input!(args as TraceFnArgs);
            add_tracing_to_fn(item_fn, args)
        }
    };
    result.unwrap_or_else(|e| e.to_compile_error()).into()
}
