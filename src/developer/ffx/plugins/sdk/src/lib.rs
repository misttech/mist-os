// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context, Result};
use ffx_config::{ConfigLevel, EnvironmentContext};
use ffx_sdk_args::{SdkCommand, SetCommand, SetRootCommand, SetSubCommand, SubCommand};
use fho::{FfxMain, FfxTool, SimpleWriter};
use sdk::{in_tree_sdk_version, Sdk, SdkVersion};
use std::io::Write;

#[derive(FfxTool)]
pub struct SdkTool {
    context: EnvironmentContext,
    sdk: fho::Result<Sdk>,
    #[command]
    cmd: SdkCommand,
}

fho::embedded_plugin!(SdkTool);

#[async_trait::async_trait(?Send)]
impl FfxMain for SdkTool {
    type Writer = SimpleWriter;

    async fn main(self, writer: Self::Writer) -> fho::Result<()> {
        match &self.cmd.sub {
            SubCommand::Version(_) => exec_version(self.sdk?, writer).await.map_err(Into::into),
            SubCommand::Set(cmd) => exec_set(self.context, cmd).await.map_err(Into::into),
        }
    }
}

async fn exec_version<W: Write>(sdk: Sdk, mut writer: W) -> Result<()> {
    match sdk.get_version() {
        SdkVersion::Version(v) => writeln!(writer, "{}", v)?,
        SdkVersion::InTree => writeln!(writer, "{}", in_tree_sdk_version())?,
        SdkVersion::Unknown => writeln!(writer, "<unknown>")?,
    }

    Ok(())
}

async fn exec_set(context: EnvironmentContext, cmd: &SetCommand) -> Result<()> {
    match &cmd.sub {
        SetSubCommand::Root(SetRootCommand { path }) => {
            let abs_path =
                path.canonicalize().context(format!("making path absolute: {:?}", path))?;
            context
                .query("sdk.root")
                .level(Some(ConfigLevel::User))
                .set(abs_path.to_string_lossy().into())
                .await?;
            Ok(())
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_version_with_string() {
        let mut out = Vec::new();
        let sdk = Sdk::get_empty_sdk_with_version(SdkVersion::Version("Test.0".to_owned()));

        exec_version(sdk, &mut out).await.unwrap();
        let out = String::from_utf8(out).unwrap();

        assert_eq!("Test.0\n", out);
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_version_in_tree() {
        let mut out = Vec::new();
        let sdk = Sdk::get_empty_sdk_with_version(SdkVersion::InTree);

        exec_version(sdk, &mut out).await.unwrap();
        let out = String::from_utf8(out).unwrap();

        let re = regex::Regex::new(r"^\d+.99991231.0.1\n$").expect("creating regex");
        assert!(re.is_match(&out));
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_version_unknown() {
        let mut out = Vec::new();
        let sdk = Sdk::get_empty_sdk_with_version(SdkVersion::Unknown);

        exec_version(sdk, &mut out).await.unwrap();
        let out = String::from_utf8(out).unwrap();

        assert_eq!("<unknown>\n", out);
    }
}
