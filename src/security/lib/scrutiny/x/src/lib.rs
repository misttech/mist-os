// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod api;
pub mod blob;
pub mod boxed;
pub mod component;
pub mod component_capability;
pub mod component_instance;
pub mod component_instance_capability;
pub mod component_manager;
pub mod component_resolver;
pub mod data_source;
pub mod hash;
pub mod package;
pub mod package_resolver;
pub mod product_bundle;
pub mod scrutiny;
pub mod system;
pub mod todo;
pub mod update_package;
pub mod zbi;

// Directly expose all API types from top-level crate.
pub use api::*;

// Expose select configuration types needed to instantiate scrutiny objects.
pub use product_bundle::ProductBundleBuilder;
pub use product_bundle::SystemSlot;
pub use scrutiny::ScrutinyBuilder;
