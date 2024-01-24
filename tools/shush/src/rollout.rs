// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::{fs::File, io::BufReader, path::Path};

use anyhow::Result;

use crate::{api::Api, issues::Issue};

pub fn rollout(api: &mut (impl Api + ?Sized), rollout_path: &Path, verbose: bool) -> Result<()> {
    let created_issues =
        serde_json::from_reader::<_, Vec<Issue>>(BufReader::new(File::open(rollout_path)?))?;

    println!("Rolling out {} lints...", created_issues.len());

    Issue::rollout(created_issues, api, verbose)?;

    Ok(())
}
