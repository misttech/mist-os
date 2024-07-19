// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod collector;
pub use collector::UnifiedCollector;

use crate::additional_boot_args::controller::*;
use crate::core::controller::blob::*;
use crate::core::controller::component::*;
use crate::core::controller::package::*;
use crate::search::controller::components::*;
use crate::search::controller::packages::*;
use crate::static_pkgs::controller::*;
use crate::verify::controller::build::*;
use crate::verify::controller::capability_routing::*;
use crate::verify::controller::structured_config::*;
use crate::zbi::controller::*;

use scrutiny::prelude::*;
use std::sync::Arc;

pub struct UnifiedPlugin {}

impl UnifiedPlugin {
    pub fn with_model() -> PluginHooks {
        PluginHooks::new(
            Arc::new(UnifiedCollector::default()),
            controllers! {
                "/devmgr/config" => ExtractAdditionalBootConfigController::default(),
                "/component" => ComponentGraphController::default(),
                "/components" => ComponentsGraphController::default(),
                "/component/manifest" => ComponentManifestGraphController::default(),
                "/packages" => PackagesGraphController::default(),
                "/blob" => BlobController::default(),
                "/search/components" => ComponentSearchController::default(),
                "/search/packages" => PackageSearchController::default(),
                "/static/pkgs" => ExtractStaticPkgsController::default(),
                "/verify/build" => VerifyBuildController::default(),
                "/verify/v2_component_model" => V2ComponentModelMappingController::default(),
                "/verify/structured_config" => VerifyStructuredConfigController::default(),
                "/zbi/sections" => ZbiSectionsController::default(),
            },
        )
    }
}
