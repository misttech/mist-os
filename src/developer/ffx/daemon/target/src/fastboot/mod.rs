// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{
    fastboot::{
        client::{FastbootImpl, InterfaceFactory},
        network::{
            tcp::TcpNetworkFactory,
            udp::{UdpNetworkFactory, UdpNetworkInterface},
        },
    },
    target::Target,
};
use anyhow::{anyhow, bail, Context, Result};
use async_trait::async_trait;
use chrono::Duration;
use fastboot::{
    command::{ClientVariable, Command},
    download,
    reply::Reply,
    send, send_with_listener, send_with_timeout, upload, SendError,
};
use ffx_config::get;
use ffx_daemon_events::FastbootInterface;
use ffx_fastboot::transport::tcp::TcpNetworkInterface;
use fidl::endpoints::ClientEnd;
use fidl_fuchsia_developer_ffx::{
    FastbootRequestStream, UploadProgressListenerMarker, UploadProgressListenerProxy,
    VariableListenerMarker, VariableListenerProxy,
};
use futures::{
    io::{AsyncRead, AsyncWrite},
    TryStreamExt,
};
use std::{convert::TryInto, fs::read, rc::Rc};
use usb_bulk::AsyncInterface as Interface;

pub mod client;
pub mod network;

/// The timeout rate in mb/s when communicating with the target device
const FLASH_TIMEOUT_RATE: &str = "fastboot.flash.timeout_rate";
/// The minimum flash timeout (in seconds) for flashing to a target device
const MIN_FLASH_TIMEOUT: &str = "fastboot.flash.min_timeout_secs";

/// Disables fastboot usb discovery if set to true.
const FASTBOOT_USB_DISCOVERY_DISABLED: &str = "fastboot.usb.disabled";

/// Fastboot Service that handles communicating with a target over the Fastboot protocol.
///
/// Since this service can handle establishing communication with a target in any state (Product,
/// Fastboot, or Zedboot) the service contains both an implementation of the USB transport and
/// the UDP transport. It is impossible to know which transport will be needed if the target
/// starts in the Product or Zedboot state so both implementations are present from creation.
pub struct Fastboot {
    target: Rc<Target>,
    usb: FastbootImpl<Interface>,
    udp: FastbootImpl<UdpNetworkInterface>,
    tcp: FastbootImpl<TcpNetworkInterface>,
}

impl Fastboot {
    pub fn new(target: Rc<Target>) -> Self {
        Self {
            target: target.clone(),
            udp: FastbootImpl::new(target.clone(), Box::new(UdpNetworkFactory::new())),
            usb: FastbootImpl::new(target.clone(), Box::new(UsbFactory::default())),
            tcp: FastbootImpl::new(target.clone(), Box::new(TcpNetworkFactory::new())),
        }
    }

    #[tracing::instrument(skip(self, stream))]
    pub async fn handle_fastboot_requests_from_stream(
        &mut self,
        mut stream: FastbootRequestStream,
    ) -> Result<()> {
        while let Some(req) = stream.try_next().await? {
            tracing::debug!("Got fastboot request: {:#?}", req);
            if let Some((_, interface)) = self.target.fastboot_address() {
                match interface {
                    FastbootInterface::Tcp => match self.tcp.handle_fastboot_request(req).await {
                        Ok(_) => (),
                        Err(e) => {
                            self.tcp.clear_interface().await;
                            return Err(e);
                        }
                    },
                    FastbootInterface::Udp => match self.udp.handle_fastboot_request(req).await {
                        Ok(_) => (),
                        Err(e) => {
                            self.udp.clear_interface().await;
                            return Err(e);
                        }
                    },
                    _ => bail!("Unexpected interface type {:?}", self.target.fastboot_interface()),
                }
            } else {
                match self.usb.handle_fastboot_request(req).await {
                    Ok(_) => (),
                    Err(e) => {
                        self.usb.clear_interface().await;
                        return Err(e);
                    }
                }
            }
        }
        // Make sure that the serial numbers are no longer in use.
        // TODO(https://fxbug.dev/123749): Replace these with a drop guard.
        futures::future::join3(
            self.tcp.clear_interface(),
            self.udp.clear_interface(),
            self.usb.clear_interface(),
        )
        .await;
        Ok(())
    }
}

#[derive(Default)]
struct UsbFactory {
    serial: Option<String>,
}

#[async_trait(?Send)]
impl InterfaceFactory<Interface> for UsbFactory {
    async fn open(&mut self, target: &Target) -> Result<Interface> {
        let (s, iface) =
            target.usb().await.context("UsbFactory cannot open target's usb interface")?;
        self.serial.replace(s.clone());
        tracing::debug!("serial now in use: {s}");
        Ok(iface)
    }

    async fn close(&self) {
        if let Some(s) = &self.serial {
            tracing::debug!("dropping in use serial: {s}");
        }
    }

    async fn is_target_discovery_enabled(&self) -> bool {
        is_usb_discovery_enabled().await
    }
}

impl Drop for UsbFactory {
    fn drop(&mut self) {
        futures::executor::block_on(async move {
            self.close().await;
        });
    }
}

pub async fn is_usb_discovery_enabled() -> bool {
    get(FASTBOOT_USB_DISCOVERY_DISABLED).await.unwrap_or(false)
}

//TODO(fxbug.dev/52733) - this info will probably get rolled into the target struct
#[derive(Debug)]
pub struct FastbootDevice {
    pub product: String,
    pub serial: String,
}

pub struct VariableListener(VariableListenerProxy);

impl VariableListener {
    fn new(listener: ClientEnd<VariableListenerMarker>) -> Result<Self> {
        Ok(Self(listener.into_proxy().map_err(|e| anyhow!(e))?))
    }
}

#[async_trait]
impl fastboot::InfoListener for VariableListener {
    async fn on_info(&self, info: String) -> Result<()> {
        if let Some((name, val)) = info.split_once(':') {
            self.0.on_variable(name.trim(), val.trim()).map_err(|e| anyhow!(e))?;
        }
        Ok(())
    }
}

pub struct UploadProgressListener(UploadProgressListenerProxy);

impl UploadProgressListener {
    fn new(listener: ClientEnd<UploadProgressListenerMarker>) -> Result<Self> {
        Ok(Self(listener.into_proxy().map_err(|e| anyhow!(e))?))
    }
}

#[async_trait]
impl fastboot::UploadProgressListener for UploadProgressListener {
    async fn on_started(&self, size: usize) -> Result<()> {
        self.0.on_started(size.try_into()?).map_err(|e| anyhow!(e))
    }

    async fn on_progress(&self, bytes_written: u64) -> Result<()> {
        self.0.on_progress(bytes_written).map_err(|e| anyhow!(e))
    }

    async fn on_error(&self, error: &str) -> Result<()> {
        self.0.on_error(error).map_err(|e| anyhow!(e))
    }

    async fn on_finished(&self) -> Result<()> {
        self.0.on_finished().map_err(|e| anyhow!(e))
    }
}

pub async fn get_all_vars<T: AsyncRead + AsyncWrite + Unpin>(
    interface: &mut T,
    listener: &VariableListener,
) -> Result<()> {
    send_with_listener(Command::GetVar(ClientVariable::All), interface, listener).await.and_then(
        |r| match r {
            Reply::Okay(_) => Ok(()),
            Reply::Fail(s) => bail!("Failed to get all variables {}", s),
            _ => bail!("Unexpected reply from fastboot deviced for get_var"),
        },
    )
}

pub async fn get_var<T: AsyncRead + AsyncWrite + Unpin>(
    interface: &mut T,
    name: &String,
) -> Result<String> {
    send(Command::GetVar(ClientVariable::Oem(name.to_string())), interface).await.and_then(|r| {
        match r {
            Reply::Okay(v) => Ok(v),
            Reply::Fail(s) => bail!("Failed to get {}: {}", name, s),
            _ => bail!("Unexpected reply from fastboot deviced for get_var"),
        }
    })
}

pub async fn stage<T: AsyncRead + AsyncWrite + Unpin>(
    interface: &mut T,
    file: &String,
    listener: &UploadProgressListener,
) -> Result<()> {
    let bytes = read(file)?;
    tracing::debug!("uploading file size: {}", bytes.len());
    match upload(&bytes[..], interface, listener).await.context(format!("uploading {}", file))? {
        Reply::Okay(s) => {
            tracing::debug!("Received response from download command: {}", s);
            Ok(())
        }
        Reply::Fail(s) => bail!("Failed to upload {}: {}", file, s),
        _ => bail!("Unexpected reply from fastboot device for download"),
    }
}

#[tracing::instrument(skip(interface, listener))]
pub async fn flash<T: AsyncRead + AsyncWrite + Unpin>(
    interface: &mut T,
    file: &String,
    name: &String,
    listener: &UploadProgressListener,
) -> Result<()> {
    let bytes = read(file)?;
    let upload_reply =
        upload(&bytes[..], interface, listener).await.context(format!("uploading {}", file))?;
    match upload_reply {
        Reply::Okay(s) => tracing::debug!("Received response from download command: {}", s),
        Reply::Fail(s) => bail!("Failed to upload {}: {}", file, s),
        _ => bail!("Unexpected reply from fastboot device for download: {:?}", upload_reply),
    };
    //timeout rate is in mb per seconds
    let min_timeout: i64 = get(MIN_FLASH_TIMEOUT).await?;
    let timeout_rate: i64 = get(FLASH_TIMEOUT_RATE).await?;
    let megabytes = (bytes.len() / 1000000) as i64;
    let mut timeout = megabytes / timeout_rate;
    timeout = std::cmp::max(timeout, min_timeout);
    tracing::debug!("Estimated timeout: {}s for {}MB", timeout, megabytes);
    let span = tracing::span!(tracing::Level::INFO, "device_flash").entered();
    let send_reply =
        send_with_timeout(Command::Flash(name.to_string()), interface, Duration::seconds(timeout))
            .await
            .context("sending flash");
    drop(span);
    match send_reply {
        Ok(Reply::Okay(_)) => Ok(()),
        Ok(Reply::Fail(s)) => bail!("Failed to flash \"{}\": {}", name, s),
        Ok(_) => bail!("Unexpected reply from fastboot device for flash command"),
        Err(ref e) => {
            if let Some(ffx_err) = e.downcast_ref::<SendError>() {
                match ffx_err {
                    SendError::Timeout => {
                        if timeout_rate == 1 {
                            bail!("Could not read response from device.  Reply timed out.");
                        }
                        let lowered_rate = timeout_rate - 1;
                        let timeout_err = format!(
                            "Time out while waiting on a response from the device. \n\
                            The current timeout rate is {} mb/s.  Try lowering the timeout rate: \n\
                            ffx config set \"{}\" {}",
                            timeout_rate, FLASH_TIMEOUT_RATE, lowered_rate
                        );
                        bail!("{}", timeout_err);
                    }
                }
            }
            bail!("Unexpected reply from fastboot device for flash command: {:?}", send_reply)
        }
    }
}

pub async fn erase<T: AsyncRead + AsyncWrite + Unpin>(
    interface: &mut T,
    name: &String,
) -> Result<()> {
    let reply = send(Command::Erase(name.to_string()), interface).await.context("sending erase")?;
    match reply {
        Reply::Okay(_) => {
            tracing::debug!("Successfully erased parition: {}", name);
            Ok(())
        }
        Reply::Fail(s) => bail!("Failed to erase \"{}\": {}", name, s),
        _ => bail!("Unexpected reply from fastboot device for erase command: {:?}", reply),
    }
}

fn handle_timeout_as_okay(r: Result<Reply>) -> Result<Reply> {
    match r {
        Err(e) if matches!(e.downcast_ref::<SendError>(), Some(SendError::Timeout)) => {
            tracing::debug!("Timed out waiting for bootloader response; assuming it's okay");
            Ok(Reply::Okay("".to_string()))
        }
        _ => r,
    }
}
pub async fn boot<T: AsyncRead + AsyncWrite + Unpin>(interface: &mut T) -> Result<()> {
    // Note: the target may not successfully send a response when asked to boot,
    // so let's use a short time-out, and treat a timeout error as a success.
    let reply = handle_timeout_as_okay(
        send_with_timeout(Command::Boot, interface, Duration::seconds(3)).await,
    )
    .context("sending boot")?;
    match reply {
        Reply::Okay(_) => {
            tracing::debug!("Successfully sent boot");
            Ok(())
        }
        Reply::Fail(s) => bail!("Failed to boot: {}", s),
        _ => bail!("Unexpected reply from fastboot device for boot command: {reply:?}"),
    }
}

pub async fn reboot<T: AsyncRead + AsyncWrite + Unpin>(interface: &mut T) -> Result<()> {
    // Note: the target may not successfully send a response when asked to reboot,
    // so let's use a short time-out, and treat a timeout error as a success.
    let reply = handle_timeout_as_okay(
        send_with_timeout(Command::Reboot, interface, Duration::seconds(3)).await,
    )
    .context("sending reboot")?;
    match reply {
        Reply::Okay(_) => {
            tracing::debug!("Successfully sent reboot");
            Ok(())
        }
        Reply::Fail(s) => bail!("Failed to reboot: {}", s),
        _ => bail!("Unexpected reply from fastboot device for reboot command: {reply:?}"),
    }
}

pub async fn reboot_bootloader<T: AsyncRead + AsyncWrite + Unpin>(interface: &mut T) -> Result<()> {
    // Note: the target may not successfully send a response when asked to reboot-bootloader,
    // so let's use a short time-out, and treat a timeout error as a success.
    let reply = handle_timeout_as_okay(
        send_with_timeout(Command::RebootBootLoader, interface, Duration::seconds(3)).await,
    )
    .context("sending reboot bootloader")?;
    match reply {
        Reply::Okay(_) => {
            tracing::debug!("Successfully sent reboot bootloader");
            Ok(())
        }
        Reply::Fail(s) => bail!("Failed to reboot to bootloader: {}", s),
        _ => {
            bail!("Unexpected reply from fastboot device for reboot bootloader command: {reply:?}")
        }
    }
}

pub async fn continue_boot<T: AsyncRead + AsyncWrite + Unpin>(interface: &mut T) -> Result<()> {
    // Note: the target may not successfully send a response when asked to continue,
    // so let's use a short time-out, and treat a timeout error as a success.
    let reply = handle_timeout_as_okay(
        send_with_timeout(Command::Continue, interface, Duration::seconds(3)).await,
    )
    .context("sending continue")?;
    match reply {
        Reply::Okay(_) => {
            tracing::debug!("Successfully sent continue");
            Ok(())
        }
        Reply::Fail(s) => bail!("Failed to continue: {}", s),
        _ => bail!("Unexpected reply from fastboot device for continue command: {reply:?}"),
    }
}

pub async fn set_active<T: AsyncRead + AsyncWrite + Unpin>(
    interface: &mut T,
    slot: &String,
) -> Result<()> {
    match send(Command::SetActive(slot.to_string()), interface)
        .await
        .context("sending set_active")?
    {
        Reply::Okay(_) => {
            tracing::debug!("Successfully sent set_active");
            Ok(())
        }
        Reply::Fail(s) => bail!("Failed to set_active: {}", s),
        _ => bail!("Unexpected reply from fastboot device for set_active command"),
    }
}

pub async fn oem<T: AsyncRead + AsyncWrite + Unpin>(interface: &mut T, cmd: &String) -> Result<()> {
    match send(Command::Oem(cmd.to_string()), interface).await.context("sending oem")? {
        Reply::Okay(_) => {
            tracing::debug!("Successfully sent oem command \"{}\"", cmd);
            Ok(())
        }
        Reply::Fail(s) => bail!("Failed to oem \"{}\": {}", cmd, s),
        _ => bail!("Unexpected reply from fastboot device for oem command \"{}\"", cmd),
    }
}

pub async fn get_staged<T: AsyncRead + AsyncWrite + Unpin>(
    interface: &mut T,
    file: &String,
) -> Result<()> {
    match download(file, interface).await.context(format!("downloading to {}", file))? {
        Reply::Okay(_) => {
            tracing::debug!("Successfully downloaded to \"{}\"", file);
            Ok(())
        }
        Reply::Fail(s) => bail!("Failed to download: {}", s),
        _ => bail!("Unexpected reply from fastboot device for download command"),
    }
}
