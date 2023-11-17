// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::additional_boot_args::AdditionalBootConfigCollection,
    anyhow::{anyhow, Context, Result},
    scrutiny::{model::controller::DataController, model::model::*},
    scrutiny_utils::build_checks,
    serde::{Deserialize, Serialize},
    serde_json::{json, value::Value},
    serde_json5,
    std::{collections::HashMap, fs::read_to_string, sync::Arc},
};

/// The input to the PreSigningController is the policy to validate against.
/// The data to validate will be implicitly collected by inclusion of plugins and accessed by
/// querying the DataModel for the relevant DataCollection.
#[derive(Deserialize, Serialize)]
pub struct PreSigningRequest {
    /// Path to policy file with contents deserializable into build_checks::BuildCheckSpec.
    pub policy_path: String,
}

/// The output of the PreSigningController is a set of errors found if any of the checks fail.
#[derive(Deserialize, Serialize)]
pub struct PreSigningResponse {
    pub errors: Vec<build_checks::ValidationError>,
}

#[derive(Default)]
pub struct PreSigningController;

impl DataController for PreSigningController {
    fn query(&self, model: Arc<DataModel>, request: Value) -> Result<Value> {
        let pre_signing_request: PreSigningRequest =
            serde_json::from_value(request).context("Failed to parse PreSigningRequest")?;
        let policy_str = read_to_string(&pre_signing_request.policy_path).context(format!(
            "Failed to read policy file from {:?}",
            &pre_signing_request.policy_path
        ))?;
        let policy: build_checks::BuildCheckSpec = serde_json5::from_str(&policy_str).context(
            format!("Failed to parse policy file from {:?}", &pre_signing_request.policy_path),
        )?;

        let boot_config_model = model
            .get::<AdditionalBootConfigCollection>()
            .context("Failed to get AdditionalBootConfigCollection")?;
        if boot_config_model.errors.len() > 0 {
            return Err(anyhow!("Cannot validate additional boot args: AdditionalBootConfigCollector reported error {:?}", boot_config_model.errors[0]));
        }

        let boot_args_data = match boot_config_model.additional_boot_args.clone() {
            Some(data) => data,
            None => HashMap::new(),
        };

        let validation_errors = build_checks::validate_build_checks(policy, boot_args_data)
            .context("Failed to run validation checks")?;

        Ok(json!(PreSigningResponse { errors: validation_errors }))
    }

    fn description(&self) -> String {
        "Runs a set of checks to verify builds for signing".to_string()
    }
}
