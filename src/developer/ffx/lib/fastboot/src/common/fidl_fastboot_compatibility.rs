// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::common::fastboot_interface::RebootEvent;
use crate::common::fastboot_interface::UploadProgress;
use crate::common::fastboot_interface::*;
use anyhow::{anyhow, bail, Error, Result};
use async_trait::async_trait;
use chrono::DateTime;
use chrono::Utc;
use errors::ffx_bail;
use fidl::endpoints::create_endpoints;
use fidl::endpoints::ServerEnd;
use fidl::Error as FidlError;
use fidl_fuchsia_developer_ffx::{
    FastbootError, FastbootProxy, RebootError, RebootListenerMarker, RebootListenerRequest,
    UploadProgressListenerMarker, UploadProgressListenerRequest, VariableListenerMarker,
    VariableListenerRequest,
};
use futures::{prelude::*, try_join};
use tokio::sync::mpsc::Sender;

const REBOOT_MANUALLY: &str =
    "\nIf the device did not reboot into the Fastboot state, try rebooting \n\
     it manually and re-running the command. Otherwise, try re-running the \n\
     command.";

const TIMED_OUT: &str = "\nTimed out while waiting to rediscover device in Fastboot.";
const SEND_TARGET_REBOOT: &str = "\nFailed while sending the target a reboot signal.";
const SEND_ON_REBOOT: &str = "\nThere was an issue communication with the daemon.";
const ZEDBOOT_COMMUNICATION: &str = "\nFailed to send the Zedboot reboot signal.";
const NO_ZEDBOOT_ADDRESS: &str = "\nUnknown Zedboot address.";
const TARGET_COMMUNICATION: &str = "\nThere was an issue communication with the target";
const FASTBOOT_ERROR: &str = "\nThere was an issue sending the Fastboot reboot command";

impl FastbootInterface for FastbootProxy {}

#[async_trait(?Send)]
impl Fastboot for FastbootProxy {
    #[tracing::instrument(skip(self, listener))]
    async fn prepare(&mut self, listener: Sender<RebootEvent>) -> Result<()> {
        let (reboot_client, reboot_server) = create_endpoints::<RebootListenerMarker>();
        let mut stream = reboot_server.into_stream()?;
        try_join!(FastbootProxy::prepare(self, reboot_client).map_err(map_fidl_error), async move {
            let stream_res = stream.try_next().await?;
            if let Some(RebootListenerRequest::OnReboot { control_handle: _ }) = stream_res {
                tracing::debug!("About to send an on_reboot event");
            }
            listener.send(RebootEvent::OnReboot).await?;
            Ok(())
        })
        .and_then(|(prepare, _)| {
            tracing::debug!("Prepare done!");
            prepare.or_else(report_reboot_error)
        })
    }

    #[tracing::instrument(skip(self))]
    async fn get_var(&mut self, name: &str) -> Result<String> {
        FastbootProxy::get_var(self, name)
            .await
            .map_err(map_fidl_error)?
            .map_err(map_fastboot_error)
    }

    #[tracing::instrument(skip(self, listener))]
    async fn get_all_vars(&mut self, listener: Sender<Variable>) -> Result<()> {
        let (var_client, var_server) = create_endpoints::<VariableListenerMarker>();
        let _ = try_join!(
            FastbootProxy::get_all_vars(self, var_client).map_err(|e| {
                tracing::error!("FIDL Communication error: {}", e);
                anyhow!(
                    "There was an error communicating with the daemon. Try running\n\
                    `ffx doctor` for further diagnositcs."
                )
            }),
            handle_variables_for_fastboot(listener, var_server),
        )?;
        Ok(())
    }

    #[tracing::instrument(skip(self, listener))]
    async fn flash(
        &mut self,
        partition_name: &str,
        path: &str,
        listener: Sender<UploadProgress>,
    ) -> Result<()> {
        let (prog_client, prog_server) = create_endpoints::<UploadProgressListenerMarker>();
        try_join!(
            FastbootProxy::flash(self, partition_name, &path, prog_client).map_err(map_fidl_error),
            handle_upload(listener, prog_server)
        )
        .and_then(|(stage, _)| {
            stage.map_err(|e| anyhow!("There was an error flashing {}: {:?}", path, e))
        })
    }

    #[tracing::instrument(skip(self))]
    async fn erase(&mut self, partition_name: &str) -> Result<()> {
        FastbootProxy::erase(self, partition_name)
            .await
            .map_err(map_fidl_error)?
            .map_err(map_fastboot_error)
    }

    #[tracing::instrument(skip(self))]
    async fn boot(&mut self) -> Result<()> {
        FastbootProxy::boot(self).await.map_err(map_fidl_error)?.map_err(map_fastboot_error)
    }

    #[tracing::instrument(skip(self))]
    async fn reboot(&mut self) -> Result<()> {
        FastbootProxy::reboot(self).await.map_err(map_fidl_error)?.map_err(map_fastboot_error)
    }

    #[tracing::instrument(skip(self, listener))]
    async fn reboot_bootloader(&mut self, listener: Sender<RebootEvent>) -> Result<()> {
        let (reboot_client, reboot_server) = create_endpoints::<RebootListenerMarker>();
        let mut stream = reboot_server.into_stream()?;
        try_join!(
            FastbootProxy::reboot_bootloader(self, reboot_client).map_err(map_fidl_error),
            async move {
                if let Some(RebootListenerRequest::OnReboot { control_handle: _ }) =
                    stream.try_next().await?
                {
                    listener.send(RebootEvent::OnReboot).await?;
                    Ok(())
                } else {
                    bail!("Did not receive reboot signal");
                }
            }
        )
        .and_then(|(reboot, _)| reboot.or_else(report_reboot_error))
    }

    #[tracing::instrument(skip(self))]
    async fn continue_boot(&mut self) -> Result<()> {
        FastbootProxy::continue_boot(self)
            .await
            .map_err(map_fidl_error)?
            .map_err(map_fastboot_error)
    }

    #[tracing::instrument(skip(self))]
    async fn get_staged(&mut self, path: &str) -> Result<()> {
        FastbootProxy::get_staged(self, path)
            .await
            .map_err(map_fidl_error)?
            .map_err(map_fastboot_error)
    }

    #[tracing::instrument(skip(self, listener))]
    async fn stage(&mut self, path: &str, listener: Sender<UploadProgress>) -> Result<()> {
        let (prog_client, prog_server) = create_endpoints::<UploadProgressListenerMarker>();

        try_join!(
            FastbootProxy::stage(self, &path, prog_client).map_err(map_fidl_error),
            handle_upload(listener, prog_server)
        )
        .and_then(|(stage, _)| {
            stage.map_err(|e| anyhow!("There was an error staging {}: {:?}", path, e))
        })
    }

    #[tracing::instrument(skip(self))]
    async fn set_active(&mut self, slot: &str) -> Result<()> {
        FastbootProxy::set_active(self, slot)
            .await
            .map_err(map_fidl_error)?
            .map_err(map_fastboot_error)
    }

    #[tracing::instrument(skip(self))]
    async fn oem(&mut self, command: &str) -> Result<()> {
        FastbootProxy::oem(self, command).await.map_err(map_fidl_error)?.map_err(map_fastboot_error)
    }
}

#[tracing::instrument(skip(listener, prog_server))]
async fn handle_upload(
    listener: Sender<UploadProgress>,
    prog_server: ServerEnd<UploadProgressListenerMarker>,
) -> Result<()> {
    let mut stream = prog_server.into_stream()?;
    let mut finish_time: Option<DateTime<Utc>> = None;
    loop {
        match stream.try_next().await? {
            Some(UploadProgressListenerRequest::OnStarted { size, .. }) => {
                listener.send(UploadProgress::OnStarted { size }).await?;
            }
            Some(UploadProgressListenerRequest::OnFinished { .. }) => {
                listener.send(UploadProgress::OnFinished).await?;
                finish_time.replace(Utc::now());
            }
            Some(UploadProgressListenerRequest::OnError { error, .. }) => {
                listener.send(UploadProgress::OnError { error: anyhow!(error.clone()) }).await?;
                ffx_bail!("{}", error);
            }
            Some(UploadProgressListenerRequest::OnProgress { bytes_written, .. }) => {
                listener.send(UploadProgress::OnProgress { bytes_written }).await?;
            }
            None => return Ok(()),
        }
    }
}

#[tracing::instrument(skip(sender, var_server))]
async fn handle_variables_for_fastboot(
    sender: Sender<Variable>,
    var_server: ServerEnd<VariableListenerMarker>,
) -> Result<()> {
    let mut stream = var_server.into_stream()?;
    loop {
        match stream.try_next().await? {
            Some(VariableListenerRequest::OnVariable { name, value, .. }) => {
                sender.send(Variable { name, value }).await?;
            }
            None => return Ok(()),
        }
    }
}

pub fn map_fidl_error(e: FidlError) -> Error {
    tracing::error!("FIDL Communication error: {}", e);
    anyhow!(
        "There was an error communicating with the daemon:\n{}\n\
        Try running `ffx doctor` for further diagnositcs.",
        e
    )
}

pub fn map_fastboot_error(e: FastbootError) -> Error {
    let err_msg = get_fastboot_error_message(e);
    tracing::error!("Daemon side fastboot error: {}", err_msg);
    anyhow!(
        "The daemon encountered an error communicating over Fastboot:\n{}\n\
        Check the daemon logs for further information.",
        err_msg
    )
}

fn get_fastboot_error_message(e: FastbootError) -> String {
    let msg = match e {
        FastbootError::ProtocolError => "Error over the Fastboot protocol",
        FastbootError::CommunicationError => "Error communicating with the Fastboot target",
        FastbootError::RediscoveredError => "Target could not be rediscovered",
        FastbootError::TargetError => "Target reported an error",
        FastbootError::NonFastbootDevice => "Target not a fastboot device",
        FastbootError::RebootFailed => "Target could not reboot",
    };
    msg.to_string()
}

fn report_reboot_error(err: RebootError) -> Result<()> {
    match err {
        RebootError::TimedOut => ffx_bail!("{}{}", TIMED_OUT, REBOOT_MANUALLY),
        RebootError::FailedToSendTargetReboot => {
            ffx_bail!("{}{}", SEND_TARGET_REBOOT, REBOOT_MANUALLY)
        }
        RebootError::FailedToSendOnReboot => bail!("{}", SEND_ON_REBOOT),
        RebootError::ZedbootCommunicationError => {
            ffx_bail!("{}{}", ZEDBOOT_COMMUNICATION, REBOOT_MANUALLY)
        }
        RebootError::NoZedbootAddress => bail!("{}", NO_ZEDBOOT_ADDRESS),
        RebootError::TargetCommunication => {
            ffx_bail!("{}{}", TARGET_COMMUNICATION, REBOOT_MANUALLY)
        }
        RebootError::FastbootError => ffx_bail!("{}", FASTBOOT_ERROR),
    }
}
