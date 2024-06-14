// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::additional_boot_args::collection::AdditionalBootConfigCollection;
use anyhow::{Context, Result};
use scrutiny::model::controller::DataController;
use scrutiny::model::model::*;
use scrutiny_utils::usage::UsageBuilder;
use serde::{Deserialize, Serialize};
use serde_json::json;
use serde_json::value::Value;
use std::sync::Arc;

#[derive(Deserialize, Serialize)]
pub struct ExtractAdditionalBootConfigRequest;

#[derive(Default)]
pub struct ExtractAdditionalBootConfigController;

impl DataController for ExtractAdditionalBootConfigController {
    fn query(&self, model: Arc<DataModel>, _: Value) -> Result<Value> {
        Ok(json!(&*model.get::<AdditionalBootConfigCollection>().context(
            "Failed to read data modeled data from ZBI-extract-additional-boot-config collector"
        )?))
    }

    fn description(&self) -> String {
        "Extracts the additional boot config from a ZBI".to_string()
    }

    fn usage(&self) -> String {
        UsageBuilder::new()
            .name("additional_boot.config - Extracts additional boot config ")
            .summary("additional_boot.config")
            .description(
                "Extracts zircon boot images and retrieves the additional boot config.
  Note: Path to ZBI file is loaded from model configuration (not as a
  controller parameter) because ZBI is loaded by a collector.",
            )
            .build()
    }
}
