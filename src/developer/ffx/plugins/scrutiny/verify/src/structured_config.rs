// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context as _, Result};
use ffx_scrutiny_verify_args::structured_config::Command;
use scrutiny_frontend::scrutiny2::Scrutiny;
use std::collections::HashSet;
use std::path::PathBuf;

pub async fn verify(cmd: &Command, recovery: bool) -> Result<HashSet<PathBuf>> {
    let policy_path = &cmd.policy.to_str().context("converting policy path to string")?.to_owned();
    let artifacts = if recovery {
        Scrutiny::from_product_bundle_recovery(&cmd.product_bundle)
    } else {
        Scrutiny::from_product_bundle(&cmd.product_bundle)
    }?
    .collect()?;

    let response = artifacts.verify_structured_config(policy_path.clone())?;
    response.check_errors().with_context(|| {
        format!("checking scrutiny output for verification errors against policy in {policy_path}")
    })?;

    Ok(response.deps)
}
