// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::ssh::do_ssh;
use crate::target::choose_target;
use anyhow::Result;
use argh::FromArgs;
use mdns_discovery::{discover_targets, MDNS_PORT};
use std::time::Duration;

mod ssh;
mod target;

fn default_repository_port() -> u32 {
    8082
}

#[derive(FromArgs)]
/// ffx Remote forwarding.
struct Funnel {
    /// the remote host to forward to
    #[argh(option, short = 'h')]
    host: String,

    /// the name of the target to forward
    #[argh(option, short = 't')]
    target_name: Option<String>,

    /// the repository port to forward to the remote host
    #[argh(option, short = 'r', default = "default_repository_port()")]
    repository_port: u32,
    // TODO(colnnelson): Add additional port forwards
}

#[fuchsia_async::run_singlethreaded]
async fn main() -> Result<()> {
    let args: Funnel = argh::from_env();

    tracing::trace!("Discoving targets...");
    let targets = discover_targets(Duration::from_secs(1), MDNS_PORT).await?;

    let target = choose_target(targets, args.target_name).await?;

    tracing::debug!("Target to forward: {:?}", target);
    do_ssh(args.host, target, args.repository_port)?;
    Ok(())
}
