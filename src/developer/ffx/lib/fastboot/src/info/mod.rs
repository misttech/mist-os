// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Result};
use ffx_fastboot_interface::fastboot_interface::{FastbootInterface, Variable};
use futures::prelude::*;
use futures::try_join;
use std::io::Write;
use tokio::sync::mpsc;
use tokio::sync::mpsc::{Receiver, Sender};

/// Aggregates fastboot variables from a callback listener.
async fn handle_variables_for_fastboot<W: Write>(
    writer: &mut W,
    mut var_server: Receiver<Variable>,
) -> Result<()> {
    loop {
        match var_server.recv().await {
            Some(Variable { name, value, .. }) => {
                writeln!(writer, "{}: {}", name, value)?;
            }
            None => return Ok(()),
        }
    }
}

#[tracing::instrument(skip(writer))]
pub async fn info<W: Write, F: FastbootInterface>(
    writer: &mut W,
    fastboot_interface: &mut F,
) -> Result<()> {
    let (var_client, var_server): (Sender<Variable>, Receiver<Variable>) = mpsc::channel(1);
    let _ = try_join!(
        fastboot_interface.get_all_vars(var_client).map_err(|e| {
            tracing::error!("FIDL Communication error: {}", e);
            anyhow!(
                "There was an error communicating with the daemon. Try running\n\
                `ffx doctor` for further diagnositcs."
            )
        }),
        handle_variables_for_fastboot(writer, var_server),
    )?;
    Ok(())
}

////////////////////////////////////////////////////////////////////////////////
// tests

#[cfg(test)]
mod test {
    use super::*;
    use crate::test::setup;

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_showing_variables() -> Result<()> {
        let (_, mut proxy) = setup();
        let mut writer = Vec::<u8>::new();
        info(&mut writer, &mut proxy).await?;
        let output = String::from_utf8(writer).expect("utf-8 string");
        assert!(output.contains("test: test"));
        Ok(())
    }
}
