// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use proc_macro::TokenStream;
use quote::quote;

/// Instantiates an impl block as separate Ipv4 and Ipv6 blocks.
///
/// This macro covers the lack of stable specialization of trait implementations
/// in Rust with the addition that the [`net_types::ip::Ip`] macro has a known
/// and closed set of implementers.
///
/// It is not uncommon that we need to specialize some implementations over the
/// IP version because of significant differences in implementations. Dependents
/// of the specific split implementations have to often split the implementation
/// too, even if the implementation block itself would look identical.
///
/// Until specialization is stabilized, this macro provides a way out of this
/// bind. It should be noted, however, that if an implementation _can_ be
/// written generically then it _should_. Facilities such as
/// [`net_types::ip::GenericOverIp`] make that possible in many cases but,
/// notably, associated types remain an unsolved problem.
///
/// `instantiate_ip_impl_block` takes a single identifier argument for the type
/// in the Generics `impl` block that will take the `Ipv4` and `Ipv6` forms in
/// the duplicated emitted blocks.
///
/// Example:
///
/// ```rust
/// struct Dependency<I>(I::Addr);
///
/// impl Dependency<Ipv4> { fn execute(&self) { /* ... */} }
/// impl Dependency<Ipv6> { fn execute(&self) { /* ... */} }
///
/// trait TraitOverIp<I: Ip> {
///     fn do_something(&self);
/// }
///
/// struct Impl;
///
/// #[instantiate_ip_impl_block(I)]
/// impl<I> TraitOverIp<I> for Impl {
///     fn do_something(&self) {
///         // instantiate_ip_impl_block rewrites this impl block instantiating
///         // I with Ipv4 and Ipv6 each time, allowing us to use dependencies
///         // that might not have complete implementations over generic I: Ip.
///         let x : Dependency<I> = Default::default();
///         x.execute();
///     }
/// }
/// ```
///
/// # Limitations and caveats
///
/// 1. If and when specialization meets our goals for generic IP implementations
///    that can be specialized, we expect to be able to delete this macro.
/// 1. This being a proc macro, it is *not* capable of replacing tokens inside a
///    declarative macro.
///
#[proc_macro_attribute]
pub fn instantiate_ip_impl_block(attr: TokenStream, input: TokenStream) -> TokenStream {
    let ip_ident = syn::parse_macro_input!(attr as syn::Ident);
    let mut item = syn::parse_macro_input!(input as syn::ItemImpl);

    let params = item.generics.params.clone();
    // Clear all params and put back in the ones not referencing the IP
    // identifier.
    item.generics.params.clear();
    for param in params {
        let pass = match &param {
            syn::GenericParam::Type(param) => param.ident != ip_ident,
            _ => true,
        };
        if pass {
            item.generics.params.push(param);
        }
    }

    struct IdentifierReplacementVisitor {
        search: syn::Ident,
        replace: syn::Path,
    }

    impl syn::visit_mut::VisitMut for IdentifierReplacementVisitor {
        fn visit_path_mut(&mut self, i: &mut syn::Path) {
            let mut iter = i.segments.iter();

            // We're always replacing a single identifier, so should always be
            // the first segment of the path.
            if iter
                .next()
                .map(|syn::PathSegment { ident, arguments }| {
                    // Look for our identifier and only accept empty path
                    // arguments, we don't expect parens or angle brackets
                    // arguments on our identifier.
                    ident == &self.search && arguments.is_empty()
                })
                .unwrap_or(false)
            {
                // Build a new path, replacing the beginning with our
                // replacement path and extending with the tail of the path.
                let mut new_segments = self.replace.segments.clone();
                new_segments.extend(iter.cloned());
                i.segments = new_segments;
            } else {
                drop(iter);
            }
            syn::visit_mut::visit_path_mut(self, i)
        }
    }

    // Now simply duplicate the definition and walk the AST twice, renaming
    // everything matching the identifier.
    let mut v4 = item.clone();
    let mut v6 = item;

    syn::visit_mut::visit_item_impl_mut(
        &mut IdentifierReplacementVisitor {
            search: ip_ident.clone(),
            replace: syn::parse_quote!(net_types::ip::Ipv4),
        },
        &mut v4,
    );
    syn::visit_mut::visit_item_impl_mut(
        &mut IdentifierReplacementVisitor {
            search: ip_ident.clone(),
            replace: syn::parse_quote!(net_types::ip::Ipv6),
        },
        &mut v6,
    );

    quote! { #v4 #v6 }.into()
}
