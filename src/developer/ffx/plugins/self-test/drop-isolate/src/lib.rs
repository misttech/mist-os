// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::*;
use ffx_core::ffx_plugin;
use ffx_selftest_drop_isolate_args::DropIsolateCommand;
use std::io::BufRead;

#[ffx_plugin("selftest.drop-isolate")]
pub async fn drop_isolate(_cmd: DropIsolateCommand) -> Result<()> {
    let ssh_key = ffx_config::get::<String, _>("ssh.priv").await?.into();
    let context = ffx_config::global_env_context().context("No global context")?;
    let isolate = ffx_isolate::Isolate::new_with_sdk("dropping-isolate", ssh_key, &context).await?;

    // manually start a daemon
    isolate.ffx(&["daemon", "echo"]).await.unwrap();

    // forward that daemon's socket info to the caller
    let socket_output =
        isolate.ffx(&["--machine", "json", "daemon", "socket"]).await.unwrap().stdout;
    println!("{socket_output}");

    for line in std::io::stdin().lock().lines() {
        if line.unwrap() == "drop isolate please" {
            // TODO(https://fxbug.dev/128451) Make this a panic once we build with panic=unwind so
            // that the test more accurately guards against the scenario that prompted writing it.
            // Returning here will drop the isolate and trigger cleanup but we're more concerned
            // about what happens when a test panics or is killed.
            return Ok(());
        }
    }
    unreachable!();
}
