// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod collection;
mod collector;
mod controller;

pub use collection::Zbi;

use {
    collector::ZbiCollector,
    controller::{
        BootFsFilesController, BootFsPackagesController, ZbiCmdlineController,
        ZbiSectionsController,
    },
    scrutiny::prelude::*,
    std::sync::Arc,
};

plugin!(
    ZbiPlugin,
    PluginHooks::new(
        collectors! {
            "ZbiCollector" => ZbiCollector::default(),
        },
        controllers! {
            "/zbi/bootfs" => BootFsFilesController::default(),
            "/zbi/bootfs_packages" => BootFsPackagesController::default(),
            "/zbi/cmdline" => ZbiCmdlineController::default(),
            "/zbi/sections" => ZbiSectionsController::default(),
        }
    ),
    vec![]
);
