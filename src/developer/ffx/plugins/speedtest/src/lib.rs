// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::num::NonZeroU32;
use std::time::Duration;

use component_debug::lifecycle::{self, CreateError};
use errors::ffx_error;
use ffx_speedtest_args::{Ping, Socket, SpeedtestCommand, Subcommand};
use ffx_writer::{SimpleWriter, ToolIO as _};
use fho::{FfxMain, FfxTool};
use fuchsia_url::AbsoluteComponentUrl;
use moniker::Moniker;
use speedtest::client;
use target_holders::RemoteControlProxyHolder;
use {fidl_fuchsia_developer_ffx_speedtest as fspeedtest, fuchsia_async as fasync};

#[derive(FfxTool)]
pub struct SpeedtestTool {
    remote_control: RemoteControlProxyHolder,
    #[command]
    cmd: SpeedtestCommand,
}

fho::embedded_plugin!(SpeedtestTool);

#[async_trait::async_trait(?Send)]
impl FfxMain for SpeedtestTool {
    type Writer = SimpleWriter;

    async fn main(self, mut writer: Self::Writer) -> fho::Result<()> {
        let Self { cmd, remote_control } = self;
        let moniker = Moniker::parse_str("/core/ffx-laboratory:speedtest").unwrap();
        let proxy = start_speedtest_component(&moniker, &remote_control).await?;
        let client = client::Client::new(proxy).await.map_err(|e| fho::bug!(e))?;
        let SpeedtestCommand { mut repeat, delay, cmd } = cmd;

        loop {
            match cmd {
                Subcommand::Ping(Ping { count }) => {
                    let report = client.ping(count).await.map_err(|e| fho::bug!(e))?;
                    writer.line(report)?;
                }
                Subcommand::Socket(Socket { transfer_mb, buffer_kb, rx }) => {
                    let direction = if rx { client::Direction::Rx } else { client::Direction::Tx };
                    let params = client::SocketTransferParams {
                        direction,
                        params: client::TransferParams {
                            data_len: transfer_mb
                                .checked_mul(NonZeroU32::new(1_000_000).unwrap())
                                .ok_or_else(|| fho::user_error!("transfer too large"))?,
                            buffer_len: buffer_kb
                                .checked_mul(NonZeroU32::new(1_000).unwrap())
                                .ok_or_else(|| fho::user_error!("buffer size too large"))?,
                        },
                    };
                    let report = client.socket(params).await.map_err(|e| fho::bug!(e))?;
                    writer.line(report)?;
                }
            }

            match repeat {
                0 => {}
                1 => break,
                _ => {
                    repeat -= 1;
                }
            }

            if delay != Duration::ZERO {
                fasync::Timer::new(delay).await;
            }
        }

        Ok(())
    }
}

async fn start_speedtest_component(
    moniker: &Moniker,
    remote_control: &RemoteControlProxyHolder,
) -> fho::Result<fspeedtest::SpeedtestProxy> {
    let lifecycle_controller =
        ffx_component::rcs::connect_to_lifecycle_controller(&remote_control).await?;

    let parent = moniker.parent().unwrap();
    let leaf = moniker.leaf().unwrap();
    let child_name = leaf.name();
    let collection = leaf.collection().unwrap();
    let url = AbsoluteComponentUrl::parse("fuchsia-pkg://fuchsia.com/speedtest#meta/speedtest.cm")
        .unwrap();
    lifecycle::create_instance_in_collection(
        &lifecycle_controller,
        &parent,
        collection,
        child_name,
        &url,
        vec![],
        None,
    )
    .await
    .or_else(|e| match e {
        CreateError::InstanceAlreadyExists => Ok(()),
        e => Err(ffx_error!(e)),
    })?;

    rcs::connect_with_timeout::<fspeedtest::SpeedtestMarker>(
        Duration::from_secs(10),
        &moniker.to_string(),
        &remote_control,
    )
    .await
    .map_err(Into::into)
}
