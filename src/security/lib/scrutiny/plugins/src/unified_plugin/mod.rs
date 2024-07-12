// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod collector;
use collector::UnifiedCollector;

use crate::additional_boot_args::controller::*;
use crate::core::controller::blob::*;
use crate::core::controller::component::*;
use crate::core::controller::package::*;
use crate::core::controller::package_extract::*;
use crate::search::controller::components::*;
use crate::search::controller::package_list::*;
use crate::search::controller::packages::*;
use crate::static_pkgs::controller::*;
use crate::verify::controller::build::*;
use crate::verify::controller::capability_routing::*;
use crate::verify::controller::component_resolvers::*;
use crate::verify::controller::pre_signing::*;
use crate::verify::controller::route_sources::*;
use crate::verify::controller::structured_config::*;
use crate::zbi::controller::*;

use scrutiny::prelude::*;
use std::sync::Arc;

pub struct UnifiedPlugin {}

impl UnifiedPlugin {
    pub fn with_model() -> PluginHooks {
        PluginHooks::new(
            Some(Arc::new(UnifiedCollector::default())),
            controllers! {
                "/devmgr/config" => ExtractAdditionalBootConfigController::default(),
                "/component" => ComponentGraphController::default(),
                "/components" => ComponentsGraphController::default(),
                "/components/urls" => ComponentsUrlListController::default(),
                "/component/manifest" => ComponentManifestGraphController::default(),
                "/package/extract" => PackageExtractController::default(),
                "/packages" => PackagesGraphController::default(),
                "/packages/urls" => PackageUrlListController::default(),
                "/blob" => BlobController::default(),
                "/search/components" => ComponentSearchController::default(),
                "/search/packages" => PackageSearchController::default(),
                "/search/package/list" => PackageListController::default(),
                "/static/pkgs" => ExtractStaticPkgsController::default(),
                "/verify/build" => VerifyBuildController::default(),
                "/verify/v2_component_model" => V2ComponentModelMappingController::default(),
                "/verify/capability_routes" => CapabilityRouteController::default(),
                "/verify/pre_signing" => PreSigningController::default(),
                "/verify/route_sources" => RouteSourcesController::default(),
                "/verify/component_resolvers" => ComponentResolversController::default(),
                "/verify/structured_config" => VerifyStructuredConfigController::default(),

                // This doesn't actually verify anything, but we need the verify model to get
                // V2ComponentModel, and depending on this plugin in `toolkit` doesn't work.
                "/verify/structured_config/extract" => ExtractStructuredConfigController::default(),
                "/zbi/bootfs" => BootFsFilesController::default(),
                "/zbi/bootfs_packages" => BootFsPackagesController::default(),
                "/zbi/cmdline" => ZbiCmdlineController::default(),
                "/zbi/sections" => ZbiSectionsController::default(),
            },
        )
    }
}
