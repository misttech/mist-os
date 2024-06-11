// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod collection;
mod collector;
mod controller;

pub use collection::{
    AdditionalBootConfigCollection, AdditionalBootConfigContents, AdditionalBootConfigError,
};

use collector::AdditionalBootConfigCollector;
use controller::ExtractAdditionalBootConfigController;
use scrutiny::prelude::*;
use std::sync::Arc;

plugin!(
    AdditionalBootConfigPlugin,
    PluginHooks::new(
        collectors! {
            "AdditionalBootConfigCollector" => AdditionalBootConfigCollector::default(),
        },
        controllers! {
            "/devmgr/config" => ExtractAdditionalBootConfigController::default(),
        }
    ),
    vec![PluginDescriptor::new("ZbiPlugin")]
);
