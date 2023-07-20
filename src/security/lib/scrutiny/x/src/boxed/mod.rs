// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub(crate) mod api;
pub(crate) mod blob;
pub(crate) mod data_source;
pub(crate) mod hash;
pub(crate) mod package;
pub(crate) mod product_bundle;
pub(crate) mod scrutiny;
pub(crate) mod system;
pub(crate) mod update_package;
pub(crate) mod zbi;

// Directly expose all API types from top-level crate.
pub use api::*;

pub fn scrutiny(product_bundle_path: Box<dyn Path>) -> Result<Box<dyn Scrutiny>, scrutiny::Error> {
    let scrutiny: Box<dyn Scrutiny> = Box::new(scrutiny::Scrutiny::new(product_bundle_path)?);
    Ok(scrutiny)
}
