// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::additional_boot_args::collector::*;
use crate::core::package::collector::*;
use crate::static_pkgs::collector::*;
use crate::verify::collector::component_model::*;
use crate::zbi::collector::*;

use anyhow::Result;
use scrutiny::prelude::*;
use std::sync::Arc;

#[derive(Default)]
pub struct UnifiedCollector {
    zbi: ZbiCollector,
    additional_boot_config: AdditionalBootConfigCollector,
    static_packages: StaticPkgsCollector,
    packages: PackageDataCollector,
    components: V2ComponentModelDataCollector,
}

impl DataCollector for UnifiedCollector {
    fn collect(&self, model: Arc<DataModel>) -> Result<()> {
        // These must be ordered in this way, because they depend on each other
        // through the model.
        self.zbi.collect(model.clone())?;
        self.additional_boot_config.collect(model.clone())?;
        self.static_packages.collect(model.clone())?;
        self.packages.collect(model.clone())?;
        self.components.collect(model.clone())?;
        Ok(())
    }
}
