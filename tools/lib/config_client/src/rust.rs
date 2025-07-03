// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{normalize_field_key, SourceGenError};
use cm_rust::{ConfigChecksum, ConfigDecl, ConfigNestedValueType, ConfigValueType};
use proc_macro2::{Ident, Literal, TokenStream};
use quote::quote;
use std::str::FromStr;
use syn::parse_str;

/// Create a Rust wrapper file containing all the fields of a config declaration
pub fn create_rust_wrapper(
    config_decl: &ConfigDecl,
    fidl_library_name: String,
) -> Result<String, SourceGenError> {
    let fidl_library_name =
        format!("fidl_{}", fidl_library_name.replace('.', "_").to_ascii_lowercase());
    let fidl_library_name = parse_str::<Ident>(&fidl_library_name)
        .map_err(|source| SourceGenError::InvalidIdentifier { input: fidl_library_name, source })?;
    let ConfigChecksum::Sha256(expected_checksum) = &config_decl.checksum;

    let expected_checksum =
        expected_checksum.into_iter().map(|b| Literal::from_str(&format!("{:#04x}", b)).unwrap());

    let mut field_declarations = vec![];
    let mut field_conversions = vec![];

    let mut record_inspect_ops = vec![];
    let mut inspect_uses = vec![quote!(Node)];

    let mut needs_array_property = false;
    let mut needs_arithmetic_array_property = false;

    for field in &config_decl.fields {
        let RustTokens { decl, conversion, record_inspect } =
            get_rust_tokens(&field.key, &field.type_)?;

        if let ConfigValueType::Vector {
            nested_type: ConfigNestedValueType::String { .. }, ..
        } = &field.type_
        {
            needs_array_property = true;
        } else if let ConfigValueType::Vector { .. } = &field.type_ {
            needs_arithmetic_array_property = true;
        }

        record_inspect_ops.push(record_inspect);
        field_declarations.push(decl);
        field_conversions.push(conversion)
    }

    if needs_arithmetic_array_property {
        inspect_uses.push(quote!(ArithmeticArrayProperty))
    }
    if needs_array_property {
        inspect_uses.push(quote!(ArrayProperty))
    }

    let stream = quote! {
        use #fidl_library_name::Config as FidlConfig;
        use fidl::unpersist;
        use fuchsia_component_config::Config as ComponentConfig;
        use fuchsia_component_config::Error;
        use fuchsia_inspect::{#(#inspect_uses),*};
        use std::convert::TryInto;

        const EXPECTED_CHECKSUM: &[u8] = &[#(#expected_checksum),*];

        #[derive(Debug)]
        pub struct Config {
            #(#field_declarations),*
        }

        impl Config {
            /// Take the config startup handle and parse its contents.
            ///
            /// # Panics
            ///
            /// If the config startup handle was already taken or if it is not valid.
            pub fn take_from_startup_handle() -> Self {
                <Self as ComponentConfig>::take_from_startup_handle()
            }

            /// Parse `Self` from `vmo`.
            pub fn from_vmo(vmo: &zx::Vmo) -> Result<Self, Error> {
                <Self as ComponentConfig>::from_vmo(vmo)
            }

            /// Parse `Self` from `bytes`.
            pub fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
                <Self as ComponentConfig>::from_bytes(bytes)
            }

            pub fn record_inspect(&self, inspector_node: &Node) {
                <Self as ComponentConfig>::record_inspect(self, inspector_node)
            }
        }

        impl ComponentConfig for Config {
            /// Parse `Self` from `bytes`.
            fn from_bytes(bytes: &[u8]) -> Result<Self, Error> {
                let (checksum_len_bytes, bytes) = bytes.split_at_checked(2)
                    .ok_or(Error::TooFewBytes)?;
                let checksum_len_bytes: [u8; 2] = checksum_len_bytes.try_into()
                    .expect("previous call guaranteed 2 element slice");
                let checksum_length = u16::from_le_bytes(checksum_len_bytes) as usize;

                let (observed_checksum, bytes) = bytes.split_at_checked(checksum_length)
                    .ok_or(Error::TooFewBytes)?;
                if observed_checksum != EXPECTED_CHECKSUM {
                    return Err(Error::ChecksumMismatch {
                        expected_checksum: EXPECTED_CHECKSUM.to_vec(),
                        observed_checksum: observed_checksum.to_vec(),
                    });
                }

                let fidl_config: FidlConfig = unpersist(bytes).map_err(Error::Unpersist)?;

                Ok(Self { #(#field_conversions),* })
            }

            fn record_inspect(&self, inspector_node: &Node) {
                #(#record_inspect_ops)*
            }
        }
    };

    Ok(stream.to_string())
}

struct RustTokens {
    decl: TokenStream,
    conversion: TokenStream,
    record_inspect: TokenStream,
}

fn get_rust_tokens(key: &str, value_type: &ConfigValueType) -> Result<RustTokens, SourceGenError> {
    let identifier = normalize_field_key(key);
    let field = parse_str::<Ident>(&identifier)
        .map_err(|source| SourceGenError::InvalidIdentifier { input: key.to_string(), source })?;

    let (record_inspect, decl) = match value_type {
        ConfigValueType::Bool => (
            quote! {
                inspector_node.record_bool(#key, self.#field);
            },
            quote! {
                pub #field: bool
            },
        ),
        ConfigValueType::Uint8 => (
            quote! {
                inspector_node.record_uint(#key, self.#field as u64);
            },
            quote! {
                pub #field: u8
            },
        ),
        ConfigValueType::Uint16 => (
            quote! {
                inspector_node.record_uint(#key, self.#field as u64);
            },
            quote! {
                pub #field: u16
            },
        ),
        ConfigValueType::Uint32 => (
            quote! {
                inspector_node.record_uint(#key, self.#field as u64);
            },
            quote! {
                pub #field: u32
            },
        ),
        ConfigValueType::Uint64 => (
            quote! {
                inspector_node.record_uint(#key, self.#field);
            },
            quote! {
                pub #field: u64
            },
        ),
        ConfigValueType::Int8 => (
            quote! {
                inspector_node.record_int(#key, self.#field as i64);
            },
            quote! {
                 pub #field: i8
            },
        ),
        ConfigValueType::Int16 => (
            quote! {
                inspector_node.record_int(#key, self.#field as i64);
            },
            quote! {
                 pub #field: i16
            },
        ),
        ConfigValueType::Int32 => (
            quote! {
                inspector_node.record_int(#key, self.#field as i64);
            },
            quote! {
                 pub #field: i32
            },
        ),
        ConfigValueType::Int64 => (
            quote! {
                inspector_node.record_int(#key, self.#field);
            },
            quote! {
                 pub #field: i64
            },
        ),
        ConfigValueType::String { .. } => (
            quote! {
                inspector_node.record_string(#key, &self.#field);
            },
            quote! {
                 pub #field: String
            },
        ),
        ConfigValueType::Vector { nested_type, .. } => match nested_type {
            ConfigNestedValueType::Bool => (
                quote! {
                    let arr = inspector_node.create_uint_array(#key, self.#field.len());
                    for i in 0..self.#field.len() {
                        arr.add(i, self.#field[i] as u64);
                    }
                    inspector_node.record(arr);
                },
                quote! {
                     pub #field: Vec<bool>
                },
            ),
            ConfigNestedValueType::Uint8 => (
                quote! {
                    let arr = inspector_node.create_uint_array(#key, self.#field.len());
                    for i in 0..self.#field.len() {
                        arr.add(i, self.#field[i] as u64);
                    }
                    inspector_node.record(arr);
                },
                quote! {
                     pub #field: Vec<u8>
                },
            ),
            ConfigNestedValueType::Uint16 => (
                quote! {
                    let arr = inspector_node.create_uint_array(#key, self.#field.len());
                    for i in 0..self.#field.len() {
                        arr.add(i, self.#field[i] as u64);
                    }
                    inspector_node.record(arr);
                },
                quote! {
                     pub #field: Vec<u16>
                },
            ),
            ConfigNestedValueType::Uint32 => (
                quote! {
                    let arr = inspector_node.create_uint_array(#key, self.#field.len());
                    for i in 0..self.#field.len() {
                        arr.add(i, self.#field[i] as u64);
                    }
                    inspector_node.record(arr);
                },
                quote! {
                     pub #field: Vec<u32>
                },
            ),
            ConfigNestedValueType::Uint64 => (
                quote! {
                    let arr = inspector_node.create_uint_array(#key, self.#field.len());
                    for i in 0..self.#field.len() {
                        arr.add(i, self.#field[i]);
                    }
                    inspector_node.record(arr);
                },
                quote! {
                     pub #field: Vec<u64>
                },
            ),
            ConfigNestedValueType::Int8 => (
                quote! {
                    let arr = inspector_node.create_int_array(#key, self.#field.len());
                    for i in 0..self.#field.len() {
                        arr.add(i, self.#field[i] as i64);
                    }
                    inspector_node.record(arr);
                },
                quote! {
                     pub #field: Vec<i8>
                },
            ),
            ConfigNestedValueType::Int16 => (
                quote! {
                    let arr = inspector_node.create_int_array(#key, self.#field.len());
                    for i in 0..self.#field.len() {
                        arr.add(i, self.#field[i] as i64);
                    }
                    inspector_node.record(arr);
                },
                quote! {
                     pub #field: Vec<i16>
                },
            ),
            ConfigNestedValueType::Int32 => (
                quote! {
                    let arr = inspector_node.create_int_array(#key, self.#field.len());
                    for i in 0..self.#field.len() {
                        arr.add(i, self.#field[i] as i64);
                    }
                    inspector_node.record(arr);
                },
                quote! {
                    pub #field: Vec<i32>
                },
            ),
            ConfigNestedValueType::Int64 => (
                quote! {
                    let arr = inspector_node.create_int_array(#key, self.#field.len());
                    for i in 0..self.#field.len() {
                        arr.add(i, self.#field[i]);
                    }
                    inspector_node.record(arr);
                },
                quote! {
                    pub #field: Vec<i64>
                },
            ),
            ConfigNestedValueType::String { .. } => (
                quote! {
                    let arr = inspector_node.create_string_array(#key, self.#field.len());
                    for i in 0..self.#field.len() {
                        arr.set(i, &self.#field[i]);
                    }
                    inspector_node.record(arr);
                },
                quote! {
                    pub #field: Vec<String>
                },
            ),
        },
    };
    let conversion = quote! {
        #field: fidl_config.#field
    };
    Ok(RustTokens { decl, conversion, record_inspect })
}
