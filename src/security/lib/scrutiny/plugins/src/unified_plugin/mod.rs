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
use crate::toolkit::controller::blobfs::*;
use crate::toolkit::controller::far::*;
use crate::toolkit::controller::fvm::*;
use crate::toolkit::controller::zbi::*;
use crate::toolkit::controller::zbi_bootfs::*;
use crate::toolkit::controller::zbi_cmdline::*;
use crate::verify::controller::build::*;
use crate::verify::controller::capability_routing::*;
use crate::verify::controller::component_resolvers::*;
use crate::verify::controller::pre_signing::*;
use crate::verify::controller::route_sources::*;
use crate::verify::controller::structured_config::*;
use crate::zbi::controller::*;

use scrutiny::prelude::*;
use std::sync::Arc;

pub struct UnifiedPlugin {
    desc: PluginDescriptor,
    hooks: PluginHooks,
    deps: Vec<PluginDescriptor>,
}

impl UnifiedPlugin {
    pub fn without_model() -> Self {
        Self {
            desc: PluginDescriptor::new("UnifiedPluginNoModel".to_string()),
            hooks: PluginHooks::new(
                collectors! {},
                controllers! {
                    "/tool/blobfs/extract" => BlobFsExtractController::default(),
                    "/tool/far/extract/meta" => FarMetaExtractController::default(),
                    "/tool/fvm/extract" => FvmExtractController::default(),
                    "/tool/zbi/extract" => ZbiExtractController::default(),
                    "/tool/zbi/extract/cmdline" => ZbiExtractCmdlineController::default(),
                    "/tool/zbi/list/bootfs" => ZbiListBootfsController::default(),
                    "/tool/zbi/extract/bootfs/packages" => ZbiExtractBootfsPackageIndex::default(),
                },
            ),
            deps: vec![],
        }
    }

    pub fn with_model() -> Self {
        Self {
            desc: PluginDescriptor::new("UnifiedPlugin".to_string()),
            hooks: PluginHooks::new(
                collectors! {
                    "UnifiedCollector" => UnifiedCollector::default(),
                },
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
            ),
            deps: vec![],
        }
    }
}

impl Plugin for UnifiedPlugin {
    fn descriptor(&self) -> &PluginDescriptor {
        &self.desc
    }
    fn dependencies(&self) -> &Vec<PluginDescriptor> {
        &self.deps
    }
    fn hooks(&mut self) -> &PluginHooks {
        &self.hooks
    }
}
