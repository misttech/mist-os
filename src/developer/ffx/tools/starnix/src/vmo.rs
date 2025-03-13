// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context, Result};
use argh::{ArgsInfo, FromArgs};
use ffx_writer::SimpleWriter;
use {
    fidl_fuchsia_developer_remotecontrol as rc, fidl_fuchsia_starnix_container as fstarcontainer,
};

use std::io::Write;

use crate::common::*;

#[derive(ArgsInfo, FromArgs, Debug, PartialEq)]
#[argh(
    subcommand,
    name = "vmo",
    example = "ffx starnix vmo -k 123456",
    description = "Return all processes that have references to a file backed by a vmo with koid"
)]
pub struct StarnixVmoCommand {
    #[argh(option, short = 'k')]
    /// koid of the vmo to search for references to.
    pub koid: u64,
}

pub async fn starnix_vmo(
    command: &StarnixVmoCommand,
    rcs_proxy: &rc::RemoteControlProxy,
    mut writer: SimpleWriter,
) -> Result<()> {
    let controller_proxy = connect_to_contoller(&rcs_proxy, None).await?;

    let result = controller_proxy
        .get_vmo_references(&fstarcontainer::ControllerGetVmoReferencesRequest {
            koid: Some(command.koid),
            ..Default::default()
        })
        .await
        .context("connecting to adbd")?;

    let mut references = result.references.unwrap_or_default();
    references.sort_by(|a, b| a.pid.partial_cmp(&b.pid).unwrap());

    if references.is_empty() {
        writeln!(writer, "Didn't find any references to vmo with koid {:?}", command.koid)
            .expect("Failed to write");
    } else {
        for reference in references {
            writeln!(
                writer,
                "Pid: {:?}, Name: {:?}",
                reference.pid.unwrap(),
                reference.process_name.unwrap()
            )
            .expect("Failed to write");
        }
    }

    Ok(())
}
