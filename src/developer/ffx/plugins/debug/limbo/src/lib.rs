// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{Context, Result};
use async_trait::async_trait;
use ffx_debug_limbo_args::{LimboCommand, LimboSubCommand};
use fho::{moniker, FfxMain, FfxTool, SimpleWriter};
use fidl_fuchsia_exception::ProcessLimboProxy;
use fuchsia_zircon_status::Status;
use fuchsia_zircon_types::{ZX_ERR_NOT_FOUND, ZX_ERR_UNAVAILABLE};

#[derive(FfxTool)]
pub struct LimboTool {
    #[command]
    cmd: LimboCommand,
    #[with(moniker("/core/exceptions"))]
    limbo_proxy: ProcessLimboProxy,
}

fho::embedded_plugin!(LimboTool);

#[async_trait(?Send)]
impl FfxMain for LimboTool {
    type Writer = SimpleWriter;

    async fn main(self, writer: Self::Writer) -> fho::Result<()> {
        match self.cmd.command {
            LimboSubCommand::Status(_) => status(self.limbo_proxy, writer).await?,
            LimboSubCommand::Enable(_) => enable(self.limbo_proxy, writer).await?,
            LimboSubCommand::Disable(_) => disable(self.limbo_proxy, writer).await?,
            LimboSubCommand::List(_) => list(self.limbo_proxy, writer).await?,
            LimboSubCommand::Release(release_cmd) => {
                release(self.limbo_proxy, release_cmd.pid, writer).await?
            }
        }
        Ok(())
    }
}

async fn status<W: std::io::Write>(limbo_proxy: ProcessLimboProxy, mut writer: W) -> Result<()> {
    let active = limbo_proxy.get_active().await?;
    if active {
        writeln!(writer, "Limbo is active.")?;
    } else {
        writeln!(writer, "Limbo is not active.")?;
    }
    Ok(())
}

async fn enable<W: std::io::Write>(limbo_proxy: ProcessLimboProxy, mut writer: W) -> Result<()> {
    let active = limbo_proxy.get_active().await?;
    if active {
        writeln!(writer, "Limbo is already active.")?;
    } else {
        limbo_proxy.set_active(true).await?;
        writeln!(writer, "Activated the process limbo.")?;
    }
    Ok(())
}

async fn disable<W: std::io::Write>(limbo_proxy: ProcessLimboProxy, mut writer: W) -> Result<()> {
    let active = limbo_proxy.get_active().await?;
    if !active {
        writeln!(writer, "Limbo is already deactivated.")?;
    } else {
        limbo_proxy.set_active(false).await?;
        writeln!(
            writer,
            "Deactivated the process limbo. All contained processes have been freed."
        )?;
    }
    Ok(())
}

async fn list<W: std::io::Write>(limbo_proxy: ProcessLimboProxy, mut writer: W) -> Result<()> {
    match limbo_proxy
        .list_processes_waiting_on_exception()
        .await
        .context("FIDL error in list_processes_waiting_on_exception")?
    {
        Ok(exceptions) => {
            if exceptions.is_empty() {
                writeln!(writer, "No processes currently on limbo.")?;
            } else {
                writeln!(writer, "Processes currently on limbo:")?;
                for metadada in exceptions {
                    let info = metadada.info.expect("missing info");
                    let process_name = metadada.process_name.expect("missing process_name");
                    let thread_name = metadada.thread_name.expect("missing thread_name");
                    writeln!(
                        writer,
                        "- {} (pid: {}), thread {} (tid: {}) on exception: {:?}",
                        process_name, info.process_koid, thread_name, info.thread_koid, info.type_
                    )?;
                }
            }
        }
        Err(e) => {
            if e == ZX_ERR_UNAVAILABLE {
                writeln!(writer, "Process limbo is not active.")?;
            } else {
                writeln!(writer, "Could not list the process limbo: {:?}", Status::from_raw(e))?;
            }
        }
    }
    Ok(())
}

async fn release<W: std::io::Write>(
    limbo_proxy: ProcessLimboProxy,
    pid: u64,
    mut writer: W,
) -> Result<()> {
    match limbo_proxy.release_process(pid).await? {
        Ok(_) => writeln!(writer, "Successfully release process {} from limbo.", pid)?,
        Err(e) => match e {
            ZX_ERR_UNAVAILABLE => writeln!(writer, "Process limbo is not active.")?,
            ZX_ERR_NOT_FOUND => writeln!(writer, "Could not find pid {} in limbo.", pid)?,
            e => writeln!(
                writer,
                "Could not release process {} from limbo: {:?}",
                pid,
                Status::from_raw(e)
            )?,
        },
    }
    Ok(())
}
