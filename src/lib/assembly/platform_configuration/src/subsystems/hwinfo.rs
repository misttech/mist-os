// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subsystems::prelude::*;
use assembly_config_schema::product_config::ProductInfoConfig;

pub(crate) struct HwinfoSubsystem;
impl DefineSubsystemConfiguration<Option<ProductInfoConfig>> for HwinfoSubsystem {
    fn define_configuration(
        _context: &ConfigurationContext<'_>,
        config: &Option<ProductInfoConfig>,
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        let hwinfo = config.clone().unwrap_or(ProductInfoConfig {
            name: "default-fuchsia".into(),
            model: "default-model".into(),
            manufacturer: "default-manufacturer".into(),
        });
        builder
            .package("hwinfo")
            .component("meta/hwinfo.cm")?
            .field("product_name", hwinfo.name)?
            .field("product_model", hwinfo.model)?
            .field("product_manufacturer", hwinfo.manufacturer)?;
        Ok(())
    }
}
