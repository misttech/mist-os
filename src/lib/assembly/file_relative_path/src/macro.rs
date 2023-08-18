// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use proc_macro2::TokenStream;
use quote::{quote, quote_spanned};
use syn::spanned::Spanned;
use syn::{parse_macro_input, Data, DeriveInput};

#[proc_macro_derive(SupportsFileRelativePaths, attributes(file_relative_paths))]
pub fn supports_file_relative_paths_derive(
    input: proc_macro::TokenStream,
) -> proc_macro::TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    derive_impl(input)
}

fn derive_impl(input: DeriveInput) -> proc_macro::TokenStream {
    // Used in the quasi-quotation below as `#name`.
    let name = input.ident;

    // Add a bound `T: HeapSize` to every type parameter T.
    let generics = input.generics;
    let (impl_generics, ty_generics, where_clause) = generics.split_for_impl();

    let fields_for_resolve = handle_fields(&input.data, Operation::Resolve);
    let fields_for_relative = handle_fields(&input.data, Operation::MakeFileRelative);

    // Create the start of the implementation of the 'SupportsFileRelativePaths'
    // trait.  This will be added to in pieces.
    let expanded = quote! {
        // The generated impl.
        impl #impl_generics assembly_file_relative_path::SupportsFileRelativePaths for #name #ty_generics #where_clause {
          fn resolve_paths_from_dir(self, dir_path: impl AsRef<camino::Utf8Path>) -> anyhow::Result<Self> {
            Ok( Self {
              #fields_for_resolve
            })
          }

          fn make_paths_relative_to_dir(
            self,
            dir_path: impl AsRef<camino::Utf8Path>,
          ) -> anyhow::Result<Self> {
            Ok( Self {
              #fields_for_relative
            })
          }
        }
    };
    proc_macro::TokenStream::from(expanded)
}

enum Operation {
    Resolve,
    MakeFileRelative,
}

fn handle_fields(data: &Data, operation: Operation) -> TokenStream {
    let file_relative_path_buf_type = syn::parse_str::<syn::Type>("FileRelativePathBuf").unwrap();

    let named = &match data {
        syn::Data::Struct(data) => {
            match &data.fields {
              syn::Fields::Named(fields) => fields,
                _ => {panic!("Deriving SupportsFileRelativePaths is only supported for structs with named fields");}
            }
        }
        syn::Data::Enum(_) => {
            panic!("Enums are not supported by the SupportsFileRelativePaths derive macro.")
        }
        syn::Data::Union(_) => {
            panic!("Unions are not supported by the SupportsFileRelativePaths derive macro.")
        }
    }.named;

    TokenStream::from_iter(named.into_iter().map(|field| {
        let name = &field.ident;

        if field.ty == file_relative_path_buf_type {
          // FileRelativePathBuf fields can be directly implemented
          match operation {
            Operation::Resolve => {
              quote_spanned!{field.span()=>
                #name: assembly_file_relative_path::FileRelativePathBuf::resolve_from_dir(self.#name, &dir_path)?,
              }
            }
            Operation::MakeFileRelative => {
              quote_spanned!{field.span()=>
                #name: assembly_file_relative_path::FileRelativePathBuf::make_relative_to_dir(self.#name, &dir_path)?,
              }
            }
          }

        } else if field.attrs.iter().any(|a| a.path.is_ident("file_relative_paths")) {
          // Fields marked with '#[file_relative_paths]' implement the trait:
          match operation {
            Operation::Resolve => {
              quote_spanned!{field.span()=>
                #name: assembly_file_relative_path::SupportsFileRelativePaths::resolve_paths_from_dir(self.#name, &dir_path)?,
              }
            }
            Operation::MakeFileRelative => {
              quote_spanned!{field.span()=>
                #name: assembly_file_relative_path::SupportsFileRelativePaths::make_paths_relative_to_dir(self.#name, &dir_path)?,
              }
            }
          }
        } else {
          // other fields are passed-through directly
          quote_spanned!{field.span()=>
            #name: self.#name,
          }
        }
      }
    ))
}
