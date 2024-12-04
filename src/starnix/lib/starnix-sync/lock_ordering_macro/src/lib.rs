// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use proc_macro::TokenStream;
use std::collections::{HashMap, HashSet};
use syn::Ident;

#[derive(Clone, PartialEq, Eq, Hash)]
struct Edge {
    from: Ident,
    to: Ident,
}

impl syn::parse::Parse for Edge {
    fn parse(input: syn::parse::ParseStream<'_>) -> syn::Result<Self> {
        let from = input.parse::<syn::Ident>()?;
        input.parse::<syn::Token![=>]>()?;
        let to: syn::Ident = input.parse()?;
        let _ = input.parse::<syn::Token![,]>();
        Ok(Edge { from, to })
    }
}

struct Graph {
    levels: HashSet<Ident>,
    edges: HashSet<Edge>,
}

impl syn::parse::Parse for Graph {
    fn parse(input: syn::parse::ParseStream<'_>) -> syn::Result<Self> {
        let mut levels = HashSet::new();
        let mut edges = HashSet::new();
        while !input.is_empty() {
            let edge: Edge = input.parse()?;
            let Edge { from, to } = edge.clone();
            levels.insert(to);
            levels.insert(from);
            edges.insert(edge);
        }
        Ok(Self { levels, edges })
    }
}

/// Collect the list of all pairs of nodes where one can be reached from another.
fn build_lock_graph(
    current: &Ident,
    past: &mut Vec<Ident>,
    adj_list: &HashMap<Ident, HashSet<Ident>>,
    all_paths: &mut HashSet<Edge>,
) {
    for p in past.into_iter() {
        if p == current {
            panic!("Detected a cycle in the lock ordering graph on level {p}.");
        }
        all_paths.insert(Edge { from: p.clone(), to: current.clone() });
    }
    let node = current.clone();
    past.push(node);
    for id in &adj_list[current] {
        build_lock_graph(&id, past, adj_list, all_paths)
    }
    past.pop();
}

/// This macro takes a definition of the lock ordering graph in the form of
/// lock_ordering!{
///     Unlocked -> A,
///     A -> B,
///     Unlocked -> C,
/// }
///
/// and defines the edges as lock level, as well as implementing LockBefore<X>
/// for all the levels from which X is reachable.
#[proc_macro]
pub fn lock_ordering(input: TokenStream) -> TokenStream {
    let Graph { levels, edges } = syn::parse_macro_input!(input as Graph);
    let mut adj_list: HashMap<Ident, HashSet<Ident>> = HashMap::new();

    let mut result = proc_macro2::TokenStream::new();
    for level in levels.into_iter() {
        adj_list.insert(level.clone(), HashSet::new());
        if level != "Unlocked" {
            result.extend(quote::quote! {
                pub enum #level {}
                impl starnix_sync::LockEqualOrBefore<#level> for #level {}
            });
        }
    }
    for Edge { from, to } in edges.into_iter() {
        adj_list
            .get_mut(&from)
            .expect("Unexpected level in lock leveling graph")
            .insert(to.clone());
    }

    let unlocked_id = Ident::new("Unlocked", proc_macro2::Span::call_site());
    let mut past: Vec<Ident> = vec![];
    let mut all_edges: HashSet<Edge> = HashSet::new();
    build_lock_graph(&unlocked_id, &mut past, &adj_list, &mut all_edges);

    for Edge { from, to } in all_edges.into_iter() {
        result.extend(quote::quote! {
            impl starnix_sync::LockAfter<#from> for #to {}
        });
    }

    result.into()
}
