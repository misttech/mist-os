// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::common::{ConfigurationBuilder, ConfigurationContext};
use anyhow::Context;
use assembly_config_schema::{BuildType, FileEntry};
use std::fs::File;
use std::io::Write;

pub(crate) fn add_build_type_config_data(
    package: &str,
    context: &ConfigurationContext<'_>,
    builder: &mut dyn ConfigurationBuilder,
) -> anyhow::Result<()> {
    let build_type = match context.build_type {
        BuildType::Eng => "eng",
        BuildType::UserDebug => "userdebug",
        BuildType::User => "user",
    };
    let gendir =
        context.get_gendir().context(format!("Getting gendir for {} subsystem", package))?;
    let filepath = gendir.join(format!("{}_build_type", package));
    let mut file = File::create(&filepath).with_context(|| format!("Opening {}", &filepath))?;
    file.write_all(build_type.as_bytes()).with_context(|| format!("Writing {}", &filepath))?;
    builder
        .package(package)
        .config_data(FileEntry { source: filepath.into(), destination: "build/type".into() })?;
    Ok(())
}
