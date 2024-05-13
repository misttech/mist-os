// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Context;
use argh::{ArgsInfo, FromArgs};
use fho::{Error, SimpleWriter};
use fidl_fuchsia_developer_remotecontrol as rc;
use fidl_fuchsia_starnix_runner as fstarrunner;

use std::io::Write;

#[derive(ArgsInfo, FromArgs, Debug, PartialEq)]
#[argh(
    subcommand,
    name = "suspend",
    example = "ffx starnix suspend",
    description = "Suspend a Starnix kernel and all the processes running in it"
)]
pub struct StarnixSuspendCommand {}

#[derive(ArgsInfo, FromArgs, Debug, PartialEq)]
#[argh(
    subcommand,
    name = "resume",
    example = "ffx starnix resume",
    description = "Resume a Starnix kernel and all the processes running in it"
)]
pub struct StarnixResumeCommand {}

const TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

pub async fn starnix_suspend(
    _command: &StarnixSuspendCommand,
    rcs_proxy: &rc::RemoteControlProxy,
    mut writer: SimpleWriter,
) -> fho::Result<()> {
    let manager_proxy = rcs::connect_to_protocol::<fstarrunner::ManagerMarker>(
        TIMEOUT,
        "core/starnix_runner",
        &rcs_proxy,
    )
    .await?;

    let _result = manager_proxy
        .suspend(&fstarrunner::ManagerSuspendRequest { ..Default::default() })
        .context("suspending kernel")?;

    writeln!(writer, "Suspended kernel.").map_err(|e| Error::Unexpected(e.into()))?;

    Ok(())
}

pub async fn starnix_resume(
    _command: &StarnixResumeCommand,
    rcs_proxy: &rc::RemoteControlProxy,
    mut writer: SimpleWriter,
) -> fho::Result<()> {
    let manager_proxy = rcs::connect_to_protocol::<fstarrunner::ManagerMarker>(
        TIMEOUT,
        "core/starnix_runner",
        &rcs_proxy,
    )
    .await?;

    let _result = manager_proxy
        .resume(&fstarrunner::ManagerResumeRequest { ..Default::default() })
        .context("resuming kernel")?;

    writeln!(writer, "Resumed kernel.").map_err(|e| Error::Unexpected(e.into()))?;

    Ok(())
}
