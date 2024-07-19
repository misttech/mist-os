// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod additional_boot_args;
mod package;
mod package_reader;
mod package_types;
mod package_utils;
mod static_packages;
mod zbi;

#[cfg(test)]
mod package_test_utils;

pub mod component_model;
pub mod unified_collector;

use anyhow::Result;
use scrutiny_collection::model::DataModel;
use std::sync::Arc;

/// The `DataCollector` trait is responsible for populating the `DataModel.`
pub trait DataCollector: Send + Sync {
    fn collect(&self, model: Arc<DataModel>) -> Result<()>;
}
