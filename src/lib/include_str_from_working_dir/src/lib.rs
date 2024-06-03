// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use proc_macro2::{Literal, Span, TokenStream};
use quote::quote;
use std::{env, fs};
use syn::{Error, LitStr, Result};

/// Imports a file's content as a string like [`include_str!`] does but looks up files relative to
/// where rustc is run from. Accepts a relative file path.
///
/// # Example
/// ```ignore
/// // Project root: If rustc is run from `/path/to/project`, and the file lives at
/// // `/path/to/project/out/gen/file.json`, then it will be included.
/// const JSON1: &str = include_str_from_working_dir_path!("out/gen/file.json");
///
/// // Build root: If rustc is run from `/path/to/project/out/build`, and the file lives at
/// // `/path/to/project/out/build/gen/file.json`, then it will be included.
/// const JSON2: &str = include_str_from_working_dir_path!("gen/file.json");
/// ```
#[proc_macro]
pub fn include_str_from_working_dir_path(
    input: proc_macro::TokenStream,
) -> proc_macro::TokenStream {
    include_str_from_working_dir_path_impl(input.into())
        .unwrap_or_else(|err| err.to_compile_error())
        .into()
}

/// Imports a file's content as a string like [`include_str!`] does but looks up files relative to
/// where rustc is run from. Accepts an environment variable name.
///
/// # Example
/// ```ignore
/// // Project root: If rustc is run from `/path/to/project`, and the environment variable
/// // `JSON_PATH` contains the string "out/build/gen/file.json", then the file at
/// // `/path/to/project/out/build/gen/file.json` will be included.
/// // Note that only the env macro is supported.
/// const JSON1: &str = include_str_from_working_dir_env!("JSON_PATH");
///
/// // Build root: If rustc is run from `/path/to/project/out/build`, and the environment variable
/// // `JSON_PATH` contains the string "gen/file.json", then the file at
/// // `/path/to/project/out/build/gen/file.json` will be included.
/// const JSON2: &str = include_str_from_working_dir_env!("JSON_PATH");
/// ```
#[proc_macro]
pub fn include_str_from_working_dir_env(input: proc_macro::TokenStream) -> proc_macro::TokenStream {
    include_str_from_working_dir_env_impl(input.into())
        .unwrap_or_else(|err| err.to_compile_error())
        .into()
}

/// Imports a file's content as a reference to a byte array like
/// [`include_bytes!`] does, but looks up files relative to where rustc is run
/// from. Accepts a relative file path.
///
/// # Example
/// ```ignore
/// // Project root: If rustc is run from `/path/to/project`, and the file lives at
/// // `/path/to/project/out/gen/file.bin`, then it will be included.
/// const BIN1: &[u8] = include_bytes_from_working_dir_path!("out/gen/file.bin");
///
/// // Build root: If rustc is run from `/path/to/project/out/build`, and the file lives at
/// // `/path/to/project/out/build/gen/file.bin`, then it will be included.
/// const BIN2: &[u8] = include_bytes_from_working_dir_path!("gen/file.bin");
/// ```
#[proc_macro]
pub fn include_bytes_from_working_dir_path(
    input: proc_macro::TokenStream,
) -> proc_macro::TokenStream {
    include_bytes_from_working_dir_path_impl(input.into())
        .unwrap_or_else(|err| err.to_compile_error())
        .into()
}

/// Imports a file's content as a reference to a byte array like
/// [`include_bytes!`] does, but looks up files relative to where rustc is run
/// from. Accepts an environment variable name.
///
/// # Example
/// ```ignore
/// // Project root: If rustc is run from `/path/to/project`, and the environment variable
/// // `BIN_PATH` contains the string "out/build/gen/file.bin", then the file at
/// // `/path/to/project/out/build/gen/file.bin` will be included.
/// // Note that only the env macro is supported.
/// const BIN1: &[u8] = include_bytes_from_working_dir_env!("BIN_PATH");
///
/// // Build root: If rustc is run from `/path/to/project/out/build`, and the environment variable
/// // `BIN_PATH` contains the string "gen/file.bin", then the file at
/// // `/path/to/project/out/build/gen/file.bin` will be included.
/// const BIN2: &[u8] = include_bytes_from_working_dir_env!("BIN_PATH");
/// ```
#[proc_macro]
pub fn include_bytes_from_working_dir_env(
    input: proc_macro::TokenStream,
) -> proc_macro::TokenStream {
    include_bytes_from_working_dir_env_impl(input.into())
        .unwrap_or_else(|err| err.to_compile_error())
        .into()
}

fn path_from_path_tokens(input: TokenStream) -> Result<String> {
    let lit_str: LitStr = syn::parse2(input)?;
    Ok(lit_str.value())
}

fn path_from_env_tokens(input: TokenStream) -> Result<String> {
    let env_var: LitStr = syn::parse2(input)?;
    match env::var(env_var.value()) {
        Ok(path) => Ok(path),
        Err(err) => Err(Error::new(
            env_var.span(),
            format!("Invalid env var {:?}: {err:?}", env_var.value()),
        )),
    }
}

fn read_file_str(path: &str) -> Result<TokenStream> {
    let contents = fs::read_to_string(path).map_err(|err| {
        Error::new(Span::call_site(), format!("Unable to read file {path:?}: {err:?}"))
    })?;
    Ok(quote! { #contents })
}

fn read_file_bytes(path: &str) -> Result<TokenStream> {
    let contents = fs::read(path).map_err(|err| {
        Error::new(Span::call_site(), format!("Unable to read file {path:?}: {err:?}"))
    })?;
    let contents = Literal::byte_string(&contents);
    Ok(quote! { #contents })
}

fn include_str_from_working_dir_path_impl(input: TokenStream) -> Result<TokenStream> {
    read_file_str(&path_from_path_tokens(input)?)
}

fn include_str_from_working_dir_env_impl(input: TokenStream) -> Result<TokenStream> {
    read_file_str(&path_from_env_tokens(input)?)
}

fn include_bytes_from_working_dir_path_impl(input: TokenStream) -> Result<TokenStream> {
    read_file_bytes(&path_from_path_tokens(input)?)
}

fn include_bytes_from_working_dir_env_impl(input: TokenStream) -> Result<TokenStream> {
    read_file_bytes(&path_from_env_tokens(input)?)
}
