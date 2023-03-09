// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
//
use anyhow::Result;
use fidl_fuchsia_developer_ffx::TargetInfo;

pub(crate) async fn choose_target(
    targets: Vec<TargetInfo>,
    def: Option<String>,
) -> Result<TargetInfo> {
    // If they specified a target...
    if let Some(t) = def {
        let filtered_targets = targets
            .iter()
            .filter(|x| x.nodename.as_ref().unwrap() == &t)
            .cloned()
            .collect::<Vec<TargetInfo>>();
        let found_target = filtered_targets.first();
        if found_target.is_none() {
            anyhow::bail!("Specified target does not exist")
        }
        return Ok(found_target.cloned().unwrap());
    }

    // TODO(colnnelson): If there is more than one target available, and they have not specified
    // the target to use, prompt user to choose it
    let first = targets.first();
    match first {
        Some(f) => Ok(f.clone()),
        None => anyhow::bail!("No targets"),
    }
}
