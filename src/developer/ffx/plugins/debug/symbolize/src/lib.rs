// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Context, Result};
use fuchsia_async::unblock;
use std::process::Command;

#[ffx_core::ffx_plugin()]
pub async fn symbolize(cmd: ffx_debug_symbolize_args::SymbolizeCommand) -> Result<()> {
    let sdk = ffx_config::global_env_context()
        .context("loading global environment context")?
        .get_sdk()
        .await?;
    if let Err(e) = symbol_index::ensure_symbol_index_registered(&sdk).await {
        eprintln!("ensure_symbol_index_registered failed, error was: {:#?}", e);
    }

    let symbolizer_path = sdk.get_host_tool("symbolizer")?;
    let mut args = cmd.symbolizer_args;
    if cmd.auth {
        args.push("--auth".to_owned());
    }

    let mut cmd = Command::new(symbolizer_path).args(args).spawn()?;

    // Return code is not used. See fxbug.dev/98220
    if let Some(_exit_code) = unblock(move || cmd.wait()).await?.code() {
        Ok(())
    } else {
        Err(anyhow!("symbolizer terminated by signal"))
    }
}
