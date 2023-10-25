// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subsystems::prelude::*;
use anyhow::Context;
use assembly_config_schema::{product_config::ProductInfoConfig, FileEntry};

pub(crate) struct HwinfoSubsystem;
impl DefineSubsystemConfiguration<Option<ProductInfoConfig>> for HwinfoSubsystem {
    fn define_configuration(
        context: &ConfigurationContext<'_>,
        config: &Option<ProductInfoConfig>,
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        let hwinfo = config.clone().unwrap_or(ProductInfoConfig {
            name: "default-fuchsia".into(),
            model: "default-model".into(),
            manufacturer: "default-manufacturer".into(),
        });

        if let Some(board_hwinfo) = &context.board_info.hardware_info {
            let contents_value = serde_json::json!({
                "name": board_hwinfo.name,
                "revision": "1"
            });
            let contents =
                serde_json::to_string_pretty(&contents_value).context("Creating BoardInfo data")?;

            let boardinfo_path = context.get_gendir()?.join("board_info.json");
            std::fs::write(&boardinfo_path, contents).context("Writing board_info.json")?;

            builder
                .package("hwinfo")
                .config_data(FileEntry {
                    source: boardinfo_path,
                    destination: "board_config.json".into(),
                })
                .context("Adding board_config.json to config_data for 'hwinfo'")?;
        }

        builder
            .package("hwinfo")
            .component("meta/hwinfo.cm")?
            .field("product_name", hwinfo.name)?
            .field("product_model", hwinfo.model)?
            .field("product_manufacturer", hwinfo.manufacturer)?;
        Ok(())
    }
}
