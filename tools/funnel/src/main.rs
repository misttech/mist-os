// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::ssh::do_ssh;
use crate::target::choose_target;
use anyhow::Result;
use argh::FromArgs;
use discovery::{wait_for_devices, DiscoverySources, TargetEvent, TargetState};
use futures::Stream;
use futures::StreamExt;
use std::{collections::HashMap, time::Duration};
use target::TargetInfo;
use timeout::timeout;
use tracing_subscriber::filter::LevelFilter;

mod logging;
mod ssh;
mod target;

fn default_log_level() -> LevelFilter {
    LevelFilter::ERROR
}

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

    /// the level to log at.
    #[argh(option, short = 'l', default = "default_log_level()")]
    log_level: LevelFilter,

    /// additional ports to forward from the remote host to the target
    #[argh(option, short = 'p')]
    additional_port_forwards: Vec<u32>,

    /// time to wait to discover targets.  
    #[argh(option, short = 'w', default = "1")]
    wait_for_target_time: u64,
}

#[fuchsia_async::run_singlethreaded]
async fn main() -> Result<()> {
    let args: Funnel = argh::from_env();

    logging::init(args.log_level)?;

    tracing::trace!("Discoving targets...");
    let wait_duration = Duration::from_secs(args.wait_for_target_time);

    // Only want added events
    let device_stream = wait_for_devices(
        |_: &_| true,
        true,
        false,
        DiscoverySources::MDNS | DiscoverySources::USB,
    )?;
    let targets = discover_target_events(device_stream, wait_duration).await?;

    let target = choose_target(targets, args.target_name).await?;

    tracing::debug!("Target to forward: {:?}", target);
    tracing::info!("Additional port forwards: {:?}", args.additional_port_forwards);
    do_ssh(args.host, target, args.repository_port, args.additional_port_forwards)?;
    Ok(())
}

async fn discover_target_events(
    mut stream: impl Stream<Item = Result<TargetEvent>> + std::marker::Unpin,
    wait_time: Duration,
) -> Result<Vec<TargetInfo>> {
    let mut targets = HashMap::new();
    let task = async {
        if let Some(s) = stream.next().await {
            if let Ok(event) = s {
                match event {
                    TargetEvent::Removed(handle) => {
                        targets.remove(&handle.node_name.unwrap_or_else(|| "".to_string()));
                    }
                    TargetEvent::Added(handle) => match handle.state {
                        TargetState::Product(addr) => {
                            targets
                                .insert(handle.node_name.unwrap_or_else(|| "".to_string()), addr);
                        }
                        TargetState::Fastboot(_) => {
                            tracing::warn!("Discovered Target: {:#?} in Fastboot mode. It cannot be used with `funnel`. Skipping", handle.node_name);
                        }
                        TargetState::Zedboot => {
                            tracing::warn!("Discovered Target: {:#?} in Zedboot mode. It cannot be used with `funnel`. Skipping.", handle.node_name);
                        }
                        TargetState::Unknown => {
                            tracing::warn!("Discovered a target in an unknwn state. Skipping");
                        }
                    },
                }
            }
        }
    };
    match timeout(wait_time, task).await {
        Ok(_) => {
            tracing::warn!("Got an okay result from the discover target loop.... this shouldnt happen we should always timeout.")
        }
        Err(_) => {
            tracing::trace!("Timeout reached");
        }
    }
    let res = targets
        .iter()
        .map(|(nodename, address)| TargetInfo {
            nodename: nodename.to_string(),
            addresses: vec![*address],
        })
        .collect();
    Ok(res)
}
