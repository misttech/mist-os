// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use proc_macro2::{Ident, TokenStream};
use quote::{quote, quote_spanned};
use syn::spanned::Spanned;
use syn::{parse_macro_input, DataEnum, DeriveInput, FieldsNamed, FieldsUnnamed};

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

    let (resolve_implementation, relative_implementation) = match &input.data {
        syn::Data::Struct(data) => match &data.fields {
            syn::Fields::Named(fields) => (
                handle_struct_with_named_fields(fields, Operation::Resolve),
                handle_struct_with_named_fields(fields, Operation::MakeFileRelative),
            ),
            // Unit structs just are themselves.
            syn::Fields::Unit => (quote! {Self}, quote! {Self}),
            syn::Fields::Unnamed(_) => {
                panic!("Structs with unnamed fields are not supported.");
            }
        },
        syn::Data::Enum(data) => {
            (handle_enum(data, Operation::Resolve), handle_enum(data, Operation::MakeFileRelative))
        }
        syn::Data::Union(_) => {
            panic!("Unions are not supported by the SupportsFileRelativePaths derive macro.")
        }
    };

    // Create the start of the implementation of the 'SupportsFileRelativePaths'
    // trait.  This will be added to in pieces.
    let expanded = quote! {
        // The generated impl.
        impl #impl_generics assembly_file_relative_path::SupportsFileRelativePaths for #name #ty_generics #where_clause {
          fn resolve_paths_from_dir(self, dir_path: impl AsRef<camino::Utf8Path>) -> anyhow::Result<Self> {
            Ok( #resolve_implementation )
          }

          fn make_paths_relative_to_dir(
            self,
            dir_path: impl AsRef<camino::Utf8Path>,
          ) -> anyhow::Result<Self> {
            Ok( #relative_implementation )
          }
        }
    };

    proc_macro::TokenStream::from(expanded)
}

#[derive(Clone, Copy)]
enum Operation {
    Resolve,
    MakeFileRelative,
}

fn handle_struct_with_named_fields(fields: &FieldsNamed, operation: Operation) -> TokenStream {
    let field_names = get_field_names(fields);
    let field_impls = handle_named_fields(fields, operation);

    quote! {
      {
        // destructure
        let Self{ #field_names } = self;
        // restructure with result of implementations
        Self { #field_impls }
      }
    }
}

fn handle_enum(data_enum: &DataEnum, operation: Operation) -> TokenStream {
    let variants = TokenStream::from_iter(data_enum.variants.iter().map(|variant| {
        let name = &variant.ident;

        match &variant.fields {
            syn::Fields::Unit => {
                quote! {
                  Self::#name => Self::#name,
                }
            }
            syn::Fields::Named(fields) => {
                let field_names = get_field_names(fields);
                let field_impls = handle_named_fields(fields, operation);
                quote! {
                  Self::#name{#field_names} => Self::#name{#field_impls},
                }
            }
            syn::Fields::Unnamed(fields) => {
                let indexes = get_field_indexes(&fields);
                let impls = handle_unnamed_fields(&fields, operation);
                quote! {
                  Self::#name(#indexes) => Self::#name(#impls),
                }
            }
        }
    }));

    quote! {
      match self {
        #variants
      }
    }
}

fn get_field_names(fields: &FieldsNamed) -> TokenStream {
    let mut output = Vec::new();
    for field in &fields.named {
        if !output.is_empty() {
            output.push(quote! {,});
        }
        let name = &field.ident;
        output.push(quote! {#name})
    }

    TokenStream::from_iter(output.into_iter())
}

fn handle_named_fields(fields: &FieldsNamed, operation: Operation) -> TokenStream {
    let file_relative_path_buf_type = syn::parse_str::<syn::Type>("FileRelativePathBuf").unwrap();

    TokenStream::from_iter(fields.named.iter().map(|field| {
        let name = &field.ident;

        if field.ty == file_relative_path_buf_type {
          // FileRelativePathBuf fields can be directly implemented
          match operation {
            Operation::Resolve => {
              quote_spanned!{field.span()=>
                #name: assembly_file_relative_path::FileRelativePathBuf::resolve_from_dir(#name, &dir_path)?,
              }
            }
            Operation::MakeFileRelative => {
              quote_spanned!{field.span()=>
                #name: assembly_file_relative_path::FileRelativePathBuf::make_relative_to_dir(#name, &dir_path)?,
              }
            }
          }

        } else if field.attrs.iter().any(|a| a.path.is_ident("file_relative_paths")) {
          // Fields marked with '#[file_relative_paths]' implement the trait:
          match operation {
            Operation::Resolve => {
              quote_spanned!{field.span()=>
                #name: assembly_file_relative_path::SupportsFileRelativePaths::resolve_paths_from_dir(#name, &dir_path)?,
              }
            }
            Operation::MakeFileRelative => {
              quote_spanned!{field.span()=>
                #name: assembly_file_relative_path::SupportsFileRelativePaths::make_paths_relative_to_dir(#name, &dir_path)?,
              }
            }
          }
        } else {
          // other fields are passed-through directly
          quote_spanned!{field.span()=>
            #name,
          }
        }
      }
    ))
}

fn get_field_indexes(fields: &syn::FieldsUnnamed) -> TokenStream {
    let mut output = Vec::new();
    for (index, field) in fields.unnamed.iter().enumerate() {
        if !output.is_empty() {
            output.push(quote! {,})
        }
        let named_index = Ident::new(&format!("field_{index}"), field.span());
        output.push(quote! { #named_index })
    }
    TokenStream::from_iter(output.into_iter())
}

fn handle_unnamed_fields(fields: &FieldsUnnamed, operation: Operation) -> TokenStream {
    let file_relative_path_buf_type = syn::parse_str::<syn::Type>("FileRelativePathBuf").unwrap();

    TokenStream::from_iter(fields.unnamed.iter().enumerate().map(|(index, field)| {
      let named_index = Ident::new(&format!("field_{index}"), field.span());

      if field.ty == file_relative_path_buf_type {
        // FileRelativePathBuf fields can be directly implemented
        match operation {
          Operation::Resolve => {
            quote_spanned!{field.span()=>
              assembly_file_relative_path::FileRelativePathBuf::resolve_from_dir(#named_index, &dir_path)?,
            }
          }
          Operation::MakeFileRelative => {
            quote_spanned!{field.span()=>
              assembly_file_relative_path::FileRelativePathBuf::make_relative_to_dir(#named_index, &dir_path)?,
            }
          }
        }

      } else if field.attrs.iter().any(|a| a.path.is_ident("file_relative_paths")) {
        // Fields marked with '#[file_relative_paths]' implement the trait:
        match operation {
          Operation::Resolve => {
            quote_spanned!{field.span()=>
              assembly_file_relative_path::SupportsFileRelativePaths::resolve_paths_from_dir(#named_index, &dir_path)?,
            }
          }
          Operation::MakeFileRelative => {
            quote_spanned!{field.span()=>
              assembly_file_relative_path::SupportsFileRelativePaths::make_paths_relative_to_dir(#named_index, &dir_path)?,
            }
          }
        }
      } else {
        // other fields are passed-through directly
        quote_spanned!{field.span()=>
          #named_index,
        }
      }
    }
  ))
}
