// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Context as _, Result};
use ffx_scrutiny_verify_args::pre_signing::Command;
use scrutiny_config::{ConfigBuilder, ModelConfig};
use scrutiny_frontend::{command_builder::CommandBuilder, launcher};
use scrutiny_plugins::verify::PreSigningResponse;
use std::{collections::HashSet, path::PathBuf};

pub async fn verify(cmd: &Command, recovery: bool) -> Result<HashSet<PathBuf>> {
    let mut deps = HashSet::new();
    let policy_path =
        &cmd.policy.to_str().context("failed to convert policy PathBuf to string")?.to_owned();
    let golden_files_dir = &cmd
        .golden_files_dir
        .to_str()
        .context("failed to convert golden_files_dir PathBuf to string")?
        .to_owned();
    let command = CommandBuilder::new("verify.pre_signing")
        .param("policy_path", policy_path.clone())
        .param("golden_files_dir", golden_files_dir.clone())
        .build();
    let plugins = vec![
        "ZbiPlugin".to_string(),
        "AdditionalBootConfigPlugin".to_string(),
        "CorePlugin".to_string(),
        "StaticPkgsPlugin".to_string(),
        "VerifyPlugin".to_string(),
    ];
    let model = if recovery {
        ModelConfig::from_product_bundle_recovery(&cmd.product_bundle.clone())
    } else {
        ModelConfig::from_product_bundle(&cmd.product_bundle.clone())
    }?;
    let mut config = ConfigBuilder::with_model(model).command(command).plugins(plugins).build();
    config.runtime.logging.silent_mode = true;

    let scrutiny_output =
        launcher::launch_from_config(config).context("Failed to run verify.pre_signing")?;

    match serde_json::from_str::<PreSigningResponse>(&scrutiny_output) {
        Ok(response) => {
            if response.errors.len() > 0 {
                println!(
                    "The build has failed pre-signing checks defined by the policy file: {:?}",
                    policy_path
                );
                println!("");
                for e in response.errors {
                    println!("{}", e);
                }
                println!("");
                return Err(anyhow!("Pre-signing verification failed."));
            }
        }
        Err(serde_error) => {
            return Err(anyhow!("Failed to parse PreSigningResponse: {:?}\nPre-signing verifier did not complete successfully: {:?}", serde_error, scrutiny_output));
        }
    }

    deps.insert(cmd.policy.clone());
    Ok(deps)
}
