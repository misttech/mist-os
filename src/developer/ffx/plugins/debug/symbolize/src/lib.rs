// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Context;
use async_trait::async_trait;
use errors::{ffx_bail, ffx_error};
use ffx_writer::SimpleWriter;
use fho::{FfxMain, FfxTool};
use fuchsia_async::unblock;
use std::process::Command;

#[derive(FfxTool)]
pub struct SymbolizeTool {
    #[command]
    cmd: ffx_debug_symbolize_args::SymbolizeCommand,
}

fho::embedded_plugin!(SymbolizeTool);

#[async_trait(?Send)]
impl FfxMain for SymbolizeTool {
    type Writer = SimpleWriter;

    async fn main(self, _writer: Self::Writer) -> fho::Result<()> {
        let sdk = ffx_config::global_env_context()
            .context("loading global environment context")?
            .get_sdk()?;
        if let Err(e) = symbol_index::ensure_symbol_index_registered(&sdk) {
            eprintln!("ensure_symbol_index_registered failed, error was: {:#?}", e);
        }

        let symbolizer_path = ffx_config::get_host_tool(&sdk, "symbolizer").await?;
        let mut args = self.cmd.symbolizer_args;
        if self.cmd.auth {
            args.push("--auth".to_owned());
        }
        if !self.cmd.no_prettify {
            args.push("--prettify-backtrace".to_owned());
            args.push("--omit-module-lines".to_owned());
        }

        let mut cmd = Command::new(symbolizer_path)
            .args(args)
            .spawn()
            .map_err(|err| ffx_error!("Failed to spawn command: {err:?}"))?;

        // Return code is not used. See https://fxbug.dev/42180530
        if let Some(_exit_code) = unblock(move || cmd.wait())
            .await
            .map_err(|err| ffx_error!("Failed to wait cmd: {err:?}"))?
            .code()
        {
            Ok(())
        } else {
            ffx_bail!("symbolizer terminated by signal.")
        }
    }
}
