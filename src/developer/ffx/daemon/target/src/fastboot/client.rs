// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use crate::{
    fastboot::{
        boot, continue_boot, erase, flash, get_all_vars, get_staged, get_var, oem, reboot,
        reboot_bootloader, set_active, stage, UploadProgressListener, VariableListener,
    },
    target::Target,
    zedboot::reboot_to_bootloader,
};
use anyhow::{anyhow, Context, Result};
use async_trait::async_trait;
use async_utils::async_once::Once;
use fastboot::UploadProgressListener as _;
use ffx_config::get;
use ffx_daemon_events::{FastbootInterface, TargetConnectionState, TargetEvent};
use ffx_metrics::add_flash_partition_event;
use fidl::{prelude::*, Error as FidlError};
use fidl_fuchsia_developer_ffx::{
    FastbootError, FastbootRequest, RebootError, RebootListenerProxy,
};
use fidl_fuchsia_developer_remotecontrol::RemoteControlProxy;
use fidl_fuchsia_hardware_power_statecontrol::{AdminMarker, AdminProxy};
use fidl_fuchsia_io::OpenFlags;
use future::select;
use futures::{
    io::{AsyncRead, AsyncWrite},
    prelude::*,
    try_join,
};
use std::{net::SocketAddr, rc::Rc, time::Duration, time::Instant};

/// Timeout in seconds to wait for target after a reboot to fastboot mode
const FASTBOOT_REBOOT_RECONNECT_TIMEOUT: &str = "fastboot.reboot.reconnect_timeout";

const ADMIN_MONIKER: &str = "/bootstrap/shutdown_shim";
const FASTBOOT_PORT: u16 = 5554;

#[async_trait(?Send)]
pub trait InterfaceFactory<T: AsyncRead + AsyncWrite + Unpin> {
    async fn open(&mut self, target: &Target) -> Result<T>;
    async fn close(&self);
}

pub struct FastbootImpl<T: AsyncRead + AsyncWrite + Unpin> {
    pub target: Rc<Target>,
    pub interface: Option<T>,
    pub interface_factory: Box<dyn InterfaceFactory<T>>,
    remote_proxy: Once<RemoteControlProxy>,
    admin_proxy: Once<AdminProxy>,
}

impl<T: AsyncRead + AsyncWrite + Unpin> FastbootImpl<T> {
    pub fn new(target: Rc<Target>, interface_factory: Box<dyn InterfaceFactory<T>>) -> Self {
        Self {
            target,
            interface: None,
            interface_factory,
            remote_proxy: Once::new(),
            admin_proxy: Once::new(),
        }
    }

    pub async fn clear_interface(&mut self) {
        self.interface = None;
        self.interface_factory.close().await;
    }

    async fn interface(&mut self) -> Result<&mut T> {
        if self.interface.is_none() {
            self.interface.replace(self.interface_factory.open(&self.target).await?);
        }
        Ok(self.interface.as_mut().expect("interface interface not available"))
    }

    async fn reboot_from_zedboot(&self, listener: &RebootListenerProxy) -> Result<(), RebootError> {
        if listener.is_closed() {
            return Err(RebootError::FailedToSendOnReboot);
        }
        listener.on_reboot().map_err(|_| RebootError::FailedToSendOnReboot)?;
        match self.target.netsvc_address() {
            Some(addr) => {
                reboot_to_bootloader(addr)
                    .await
                    .map_err(|_| RebootError::ZedbootCommunicationError)?;
                self.check_for_fastboot().await
            }
            None => Err(RebootError::NoZedbootAddress),
        }
    }

    fn is_target_in_fastboot(&self) -> bool {
        match self.target.get_connection_state() {
            TargetConnectionState::Fastboot(_) => true,
            _ => false,
        }
    }

    async fn check_for_fastboot(&self) -> Result<(), RebootError> {
        let check_bootloader_fut = Box::pin(self.check_for_bootloader_fastboot());
        let check_userspace_fut = Box::pin(self.check_for_userspace_fastboot());
        match select(check_bootloader_fut, check_userspace_fut).await {
            future::Either::Left((Ok(_), _)) => Ok(()),
            future::Either::Left((_, secondfut)) => secondfut.await,
            future::Either::Right((Ok(_), _)) => Ok(()),
            future::Either::Right((_, secondfut)) => secondfut.await,
        }
    }

    async fn check_for_bootloader_fastboot(&self) -> Result<(), RebootError> {
        for _ in 0..2 {
            if self.is_target_in_fastboot() {
                return Ok(());
            }
            // Even if it times out, just check if it's in fastboot.
            let _ = self
                .target
                .events
                .wait_for(Some(Duration::from_secs(10)), |e| e == TargetEvent::Rediscovered)
                .await;

            if self.is_target_in_fastboot() {
                return Ok(());
            }
        }
        Err(RebootError::TimedOut)
    }

    async fn check_for_userspace_fastboot(&self) -> Result<(), RebootError> {
        // Try and resolve the SSH address of the device in an attempt to
        // connect to userspace fastboot. If such an address does not exist,
        // this target cannot be in userspace fastboot, so return an error.
        let mut addr: SocketAddr =
            self.target.ssh_address().ok_or(RebootError::FailedToSendTargetReboot)?.into();
        // TODO(https://fxbug.dev/123747): Remove this hardcoded port.
        addr.set_port(FASTBOOT_PORT);
        // We don't want to modify the existing target object until we're
        // certain that it's actually in userspace fastboot, so create a new
        // Target object instead.
        let tclone = Target::new_with_fastboot_addrs(
            Option::<String>::None,
            Option::<String>::None,
            vec![addr].iter().map(|x| From::from(*x)).collect(),
            FastbootInterface::Tcp,
        );
        for _ in 0..2 {
            if tclone.is_fastboot_tcp().await.unwrap_or(false) {
                // Now that we've verified that the target is in userspace
                // fastboot, we can transition it to that state. We need to
                // perform the transition explicitly, as the discovery loops
                // are not always able to detect a target in userspace
                // fastboot.
                self.target.from_manual_to_tcp_fastboot();
                return Ok(());
            }
        }
        Err(RebootError::TimedOut)
    }

    async fn reboot_from_product(&self, listener: &RebootListenerProxy) -> Result<(), RebootError> {
        if listener.is_closed() {
            return Err(RebootError::FailedToSendOnReboot);
        }
        listener.on_reboot().map_err(|_| RebootError::FailedToSendOnReboot)?;
        match self
            .get_admin_proxy()
            .await
            .map_err(|_| RebootError::TargetCommunication)?
            .reboot_to_bootloader()
            .await
        {
            Ok(_) => self.check_for_fastboot().await,
            Err(_e @ FidlError::ClientChannelClosed { .. }) => self.check_for_fastboot().await,
            Err(e) => {
                tracing::error!("FIDL Error for reboot_to_bootloader {:?}", e);
                Err(RebootError::FailedToSendTargetReboot)
            }
        }
    }

    async fn prepare_device(&self, listener: &RebootListenerProxy) -> Result<(), RebootError> {
        match self.target.get_connection_state() {
            TargetConnectionState::Fastboot(_) => Ok(()),
            TargetConnectionState::Zedboot(_) => self.reboot_from_zedboot(listener).await,
            _ => self.reboot_from_product(listener).await,
        }
    }

    async fn report_flash_metrics(
        &self,
        flash_duration: Duration,
        path: String,
        partition_name: String,
    ) -> Result<()> {
        // Check file size
        let meta = std::fs::metadata(&path);
        let file_size = meta?.len();
        // Get target info
        let (product, board) = match self.target.build_config() {
            Some(config) => (config.product_config, config.board_config),
            None => ("".to_string(), "".to_string()),
        };
        // Report
        add_flash_partition_event(&partition_name, &product, &board, file_size, &flash_duration)
            .await
    }

    pub async fn handle_fastboot_request(&mut self, req: FastbootRequest) -> Result<()> {
        tracing::debug!("fastboot - received req: {:?}", req);
        match req {
            FastbootRequest::Prepare { listener, responder } => {
                let res = future::ready(
                    listener.into_proxy().map_err(|_| RebootError::TargetCommunication),
                )
                .and_then(|proxy| async move { self.prepare_device(&proxy).await })
                .await;

                match res {
                    Ok(_) => responder.send(Ok(()))?,
                    Err(e) => {
                        tracing::error!("Error preparing device: {:?}", e);
                        responder.send(Err(e)).context("sending error response")?;
                    }
                }
            }
            FastbootRequest::GetVar { name, responder } => {
                let res = self
                    .interface()
                    .and_then(|interface| async { get_var(interface, &name).await })
                    .await;
                match res {
                    Ok(value) => responder.send(Ok(&value))?,
                    Err(e) => {
                        tracing::error!("Error getting variable '{}': {:?}", name, e);
                        responder
                            .send(Err(FastbootError::ProtocolError))
                            .context("sending error response")?;
                    }
                }
            }
            FastbootRequest::GetAllVars { listener, responder } => {
                let res = future::ready(VariableListener::new(listener))
                    .and_then(|variable_listener| async move {
                        let iface = self.interface().await?;
                        Ok((iface, variable_listener))
                    })
                    .and_then(|iface_listen| async move {
                        get_all_vars(iface_listen.0, &iface_listen.1).await
                    })
                    .await;

                match res {
                    Ok(()) => {
                        responder.send(Ok(()))?;
                    }
                    Err(e) => {
                        tracing::error!("Error getting all variables: {:?}", e);
                        responder
                            .send(Err(FastbootError::ProtocolError))
                            .context("sending error response")?;
                    }
                }
            }
            FastbootRequest::Flash { partition_name, path, listener, responder } => {
                let upload_listener_res = UploadProgressListener::new(listener);
                if let Err(ref e) = upload_listener_res {
                    tracing::error!(
                        "Error while flashing \"{}\" from {}. Cannot make Upload Listener:\n{:?}",
                        partition_name,
                        path,
                        e,
                    );
                    responder
                        .send(Err(FastbootError::ProtocolError))
                        .context("sending error response")?;
                    anyhow::bail!("Got error while creating UploadProgressListener: {}", e);
                }

                let upload_listener = upload_listener_res.unwrap();

                let flash_start = Instant::now();
                let res = future::ready(Ok(()))
                    .and_then(|_| async { self.interface().await })
                    .and_then(|interface| async {
                        flash(interface, &path, &partition_name, &upload_listener).await
                    })
                    .await;
                let flash_duration = Instant::now() - flash_start;

                match res {
                    Ok(_) => {
                        let report_res =
                            self.report_flash_metrics(flash_duration, path, partition_name).await;
                        if let Err(e) = report_res {
                            tracing::debug!("Error reporting flash partition metrics:\n{:?}", e);
                        }
                        responder.send(Ok(()))?
                    }
                    Err(e) => {
                        tracing::error!(
                            "Error flashing \"{}\" from {}:\n{:?}",
                            partition_name,
                            path,
                            e
                        );
                        upload_listener.on_error(&format!("{}", e))?;
                        responder
                            .send(Err(FastbootError::ProtocolError))
                            .context("sending error response")?;
                    }
                }
            }
            FastbootRequest::Erase { partition_name, responder } => {
                let res = future::ready(Ok(()))
                    .and_then(|_| async { self.interface().await })
                    .and_then(|interface| async { erase(interface, &partition_name).await })
                    .await;

                match res {
                    Ok(_) => responder.send(Ok(()))?,
                    Err(e) => {
                        tracing::error!("Error erasing \"{}\": {:?}", partition_name, e);
                        responder
                            .send(Err(FastbootError::ProtocolError))
                            .context("sending error response")?;
                    }
                }
            }
            FastbootRequest::Boot { responder } => {
                let res = future::ready(Ok(()))
                    .and_then(|_| async { self.interface().await })
                    .and_then(|interface| async { boot(interface).await })
                    .await;
                match res {
                    Ok(_) => {
                        self.clear_interface().await;
                        responder.send(Ok(()))?;
                    }
                    Err(e) => {
                        tracing::error!("Error booting: {:?}", e);
                        responder
                            .send(Err(FastbootError::ProtocolError))
                            .context("sending error response")?;
                    }
                }
            }
            FastbootRequest::Reboot { responder } => {
                let res = future::ready(Ok(()))
                    .and_then(|_| async { self.interface().await })
                    .and_then(|interface| async { reboot(interface).await })
                    .await;
                match res {
                    Ok(_) => {
                        self.clear_interface().await;
                        responder.send(Ok(()))?;
                    }
                    Err(e) => {
                        tracing::error!("Error rebooting: {:?}", e);
                        responder
                            .send(Err(FastbootError::ProtocolError))
                            .context("sending error response")?;
                    }
                }
            }
            FastbootRequest::RebootBootloader { listener, responder } => {
                let res = future::ready(Ok(()))
                    .and_then(|_| async { self.interface().await })
                    .and_then(|interface| async { reboot_bootloader(interface).await })
                    .await;
                match res.map_err(|_| RebootError::FastbootError) {
                    Ok(_) => {
                        let reboot_timeout: u64 =
                            get(FASTBOOT_REBOOT_RECONNECT_TIMEOUT).await.unwrap_or(10);
                        self.clear_interface().await;
                        match try_join!(
                            self.target
                                .events
                                .wait_for(Some(Duration::from_secs(reboot_timeout)), |e| {
                                    e == TargetEvent::Rediscovered
                                })
                                .map_err(|_| RebootError::TimedOut),
                            async move {
                                let proxy = listener
                                    .into_proxy()
                                    .map_err(|_| RebootError::FailedToSendOnReboot)?;
                                if proxy.is_closed() {
                                    return Err(RebootError::FailedToSendOnReboot);
                                }
                                proxy.on_reboot().map_err(|_| RebootError::FailedToSendOnReboot)
                            }
                        ) {
                            Ok(_) => {
                                tracing::debug!("Rediscovered reboot target");
                                responder.send(Ok(()))?;
                            }
                            Err(e) => {
                                tracing::error!(
                                    "Error rebooting and rediscovering target: {:?}",
                                    e
                                );
                                // Check the target and see what state it's in.  Maybe we just
                                // missed the event.
                                match self.check_for_fastboot().await {
                                    Ok(_) => {
                                        tracing::debug!("Target in fastboot despite timeout.");
                                        responder.send(Ok(()))?;
                                    }
                                    _ => {
                                        responder.send(Err(e)).context("sending error response")?
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        tracing::error!("Error rebooting: {:?}", e);
                        responder.send(Err(e)).context("sending error response")?;
                    }
                }
            }
            FastbootRequest::ContinueBoot { responder } => {
                let res = future::ready(Ok(()))
                    .and_then(|_| async { self.interface().await })
                    .and_then(|interface| async { continue_boot(interface).await })
                    .await;
                match res {
                    Ok(_) => {
                        self.clear_interface().await;
                        responder.send(Ok(()))?;
                    }
                    Err(e) => {
                        tracing::error!("Error continuing boot: {:?}", e);
                        responder
                            .send(Err(FastbootError::ProtocolError))
                            .context("sending error response")?;
                    }
                }
            }
            FastbootRequest::SetActive { slot, responder } => {
                let res = future::ready(Ok(()))
                    .and_then(|_| async { self.interface().await })
                    .and_then(|interface| async { set_active(interface, &slot).await })
                    .await;
                match res {
                    Ok(_) => responder.send(Ok(()))?,
                    Err(e) => {
                        tracing::error!("Error setting active: {:?}", e);
                        responder
                            .send(Err(FastbootError::ProtocolError))
                            .context("sending error response")?;
                    }
                }
            }
            FastbootRequest::Stage { path, listener, responder } => {
                let res = future::ready(UploadProgressListener::new(listener))
                    .and_then(|upload_listener| async {
                        let interface = self.interface().await?;
                        Ok((interface, upload_listener))
                    })
                    .and_then(|interface_listener| async move {
                        stage(interface_listener.0, &path, &interface_listener.1).await
                    })
                    .await;
                match res {
                    Ok(_) => responder.send(Ok(()))?,
                    Err(e) => {
                        tracing::error!("Error setting active: {:?}", e);
                        responder
                            .send(Err(FastbootError::ProtocolError))
                            .context("sending error response")?;
                    }
                }
            }
            FastbootRequest::Oem { command, responder } => {
                let res = future::ready(Ok(()))
                    .and_then(|_| async { self.interface().await })
                    .and_then(|interface| async { oem(interface, &command).await })
                    .await;
                match res {
                    Ok(_) => responder.send(Ok(()))?,
                    Err(e) => {
                        tracing::error!("Error sending oem \"{}\": {:?}", command, e);
                        responder
                            .send(Err(FastbootError::ProtocolError))
                            .context("sending error response")?;
                    }
                }
            }
            FastbootRequest::GetStaged { path, responder } => {
                let res = match self.interface().await {
                    Ok(iface) => get_staged(iface, &path).await,
                    Err(e) => Err(e),
                };
                match res {
                    Ok(_) => responder.send(Ok(()))?,
                    Err(e) => {
                        tracing::error!("Error getting staged file: {:?}", e);
                        responder
                            .send(Err(FastbootError::ProtocolError))
                            .context("sending error response")?;
                    }
                }
            }
        }
        Ok(())
    }

    async fn get_remote_proxy(&self) -> Result<RemoteControlProxy> {
        self.remote_proxy
            .get_or_try_init(self.target.init_remote_proxy())
            .await
            .map(|proxy| proxy.clone())
    }

    async fn get_admin_proxy(&self) -> Result<AdminProxy> {
        self.admin_proxy.get_or_try_init(self.init_admin_proxy()).await.map(|proxy| proxy.clone())
    }

    async fn init_admin_proxy(&self) -> Result<AdminProxy> {
        let (proxy, server_end) =
            fidl::endpoints::create_proxy::<AdminMarker>().map_err(|e| anyhow!(e))?;
        self.get_remote_proxy()
            .await?
            .connect_capability(
                ADMIN_MONIKER,
                AdminMarker::PROTOCOL_NAME,
                server_end.into_channel(),
                OpenFlags::empty(),
            )
            .await?
            .map(|_| proxy)
            .map_err(|_| anyhow!("could not get admin proxy"))
    }
}

////////////////////////////////////////////////////////////////////////////////
// tests

#[cfg(test)]
mod test {
    use super::*;
    use anyhow::bail;
    use fastboot::reply::Reply;
    use fidl::endpoints::{create_endpoints, create_proxy_and_stream};
    use fidl_fuchsia_developer_ffx::{
        FastbootError, FastbootMarker, FastbootProxy, RebootListenerMarker, RebootListenerRequest,
        UploadProgressListenerMarker,
    };
    use fuchsia_async::TimeoutExt;
    use futures::task::{Context as fContext, Poll};
    use serial_test::serial;
    use std::{
        io::{BufWriter, Write},
        pin::Pin,
    };
    use tempfile::NamedTempFile;

    struct TestTransport {
        replies: Vec<Reply>,
        timeout_at_end: bool,
    }

    impl AsyncRead for TestTransport {
        fn poll_read(
            self: Pin<&mut Self>,
            _cx: &mut fContext<'_>,
            buf: &mut [u8],
        ) -> Poll<std::io::Result<usize>> {
            let timeout_at_end = self.timeout_at_end;
            match self.get_mut().replies.pop() {
                Some(r) => {
                    let reply = Vec::<u8>::from(r);
                    buf[..reply.len()].copy_from_slice(&reply);
                    Poll::Ready(Ok(reply.len()))
                }
                None => {
                    if timeout_at_end {
                        Poll::Pending
                    } else {
                        Poll::Ready(Ok(0))
                    }
                }
            }
        }
    }

    impl AsyncWrite for TestTransport {
        fn poll_write(
            self: Pin<&mut Self>,
            _cx: &mut fContext<'_>,
            buf: &[u8],
        ) -> Poll<std::io::Result<usize>> {
            Poll::Ready(Ok(buf.len()))
        }

        fn poll_flush(self: Pin<&mut Self>, _cx: &mut fContext<'_>) -> Poll<std::io::Result<()>> {
            unimplemented!();
        }

        fn poll_close(self: Pin<&mut Self>, _cx: &mut fContext<'_>) -> Poll<std::io::Result<()>> {
            unimplemented!();
        }
    }

    impl TestTransport {
        pub fn new(timeout_at_end: bool) -> Self {
            TestTransport { replies: Vec::new(), timeout_at_end }
        }

        pub fn push(&mut self, reply: Reply) {
            self.replies.push(reply);
        }
    }

    struct TestFactory {
        replies: Vec<Reply>,
        timeout_at_end: bool,
    }

    impl TestFactory {
        pub fn new(replies: Vec<Reply>, timeout_at_end: bool) -> Self {
            Self { replies, timeout_at_end }
        }
    }

    #[async_trait(?Send)]
    impl InterfaceFactory<TestTransport> for TestFactory {
        async fn open(&mut self, _target: &Target) -> Result<TestTransport> {
            let mut transport = TestTransport::new(self.timeout_at_end);
            self.replies.iter().rev().for_each(|r| transport.push(r.clone()));
            return Ok(transport);
        }

        async fn close(&self) {}
    }

    async fn setup(replies: Vec<Reply>) -> (Rc<Target>, FastbootProxy) {
        setup_with_timeout(replies, false).await
    }

    async fn setup_with_timeout(
        replies: Vec<Reply>,
        timeout_at_end: bool,
    ) -> (Rc<Target>, FastbootProxy) {
        let target = Target::new_named("scooby-dooby-doo");
        let mut fb = FastbootImpl::<TestTransport>::new(
            target.clone(),
            Box::new(TestFactory::new(replies, timeout_at_end)),
        );
        let (proxy, mut stream) = create_proxy_and_stream::<FastbootMarker>().unwrap();
        fuchsia_async::Task::local(async move {
            loop {
                match stream.try_next().await {
                    Ok(Some(req)) => match fb.handle_fastboot_request(req).await {
                        Ok(_) => (),
                        Err(e) => {
                            fb.clear_interface().await;
                            tracing::error!(
                                "There was an error handling fastboot requests: {:?}",
                                e
                            );
                        }
                    },
                    Ok(None) => {
                        break;
                    }
                    Err(e) => {
                        tracing::error!("Failed to obtain next fastboot request: {:?}", e);
                        break;
                    }
                }
            }
            // Make sure the serial is no longer in use.
            fb.clear_interface().await;
            tracing::debug!("Fastboot proxy finished - client disconnected");
            assert!(fb.interface.is_none());
        })
        .detach();
        (target, proxy)
    }

    #[fuchsia_async::run_singlethreaded(test)]
    #[serial]
    async fn test_flash() -> Result<()> {
        let _env = ffx_config::test_init().await?;
        let (prog_client, prog_server) = create_endpoints::<UploadProgressListenerMarker>();
        let mut stream = prog_server.into_stream()?;
        let file: NamedTempFile = NamedTempFile::new().expect("tmp access failed");
        let mut buffer = BufWriter::new(&file);
        buffer.write_all(b"Test")?;
        buffer.flush()?;
        let (_, proxy) = setup(vec![
            Reply::Data(4),
            Reply::Okay("".to_string()), //Download Reply
            Reply::Okay("".to_string()), //Flash Reply
        ])
        .await;
        let filepath = file.path().to_str().ok_or(anyhow!("error getting tempfile path"))?;
        try_join!(
            async move {
                while let Some(_) = stream.try_next().await? { /* do nothing */ }
                Ok(())
            },
            proxy
                .flash("test", filepath, prog_client)
                .map_err(|e| anyhow!("error flashing: {:?}", e))
        )
        .and_then(|(_, flash)| {
            assert!(flash.is_ok());
            Ok(())
        })
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_flash_sends_protocol_error_after_unexpected_reply() -> Result<()> {
        let (prog_client, prog_server) = create_endpoints::<UploadProgressListenerMarker>();
        let mut stream = prog_server.into_stream()?;
        let file: NamedTempFile = NamedTempFile::new().expect("tmp access failed");
        let mut buffer = BufWriter::new(&file);
        buffer.write_all(b"Test")?;
        buffer.flush()?;
        let (_, proxy) = setup(vec![Reply::Data(6)]).await;
        let filepath = file.path().to_str().ok_or(anyhow!("error getting tempfile path"))?;
        try_join!(
            async move {
                while let Some(_) = stream.try_next().await? { /* do nothing */ }
                Ok(())
            },
            proxy
                .flash("test", filepath, prog_client)
                .map_err(|e| anyhow!("error flashing: {:?}", e))
        )
        .and_then(|(_, flash)| {
            assert_eq!(flash.err(), Some(FastbootError::ProtocolError));
            Ok(())
        })
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_erase() -> Result<()> {
        let (_, proxy) = setup(vec![Reply::Okay("".to_string())]).await;
        proxy.erase("test").await?.map_err(|e| anyhow!("error erase: {:?}", e))
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_erase_sends_protocol_error_after_unexpected_reply() -> Result<()> {
        let (_, proxy) = setup(vec![Reply::Fail("".to_string())]).await;
        let res = proxy.erase("test").await?;
        assert_eq!(res.err(), Some(FastbootError::ProtocolError));
        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_reboot() -> Result<()> {
        let (_, proxy) = setup(vec![Reply::Okay("".to_string())]).await;
        proxy.reboot().await?.map_err(|e| anyhow!("error reboot: {:?}", e))
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_reboot_sends_protocol_error_after_unexpected_reply() -> Result<()> {
        let (_, proxy) = setup(vec![Reply::Fail("".to_string())]).await;
        let res = proxy.reboot().await?;
        assert_eq!(res.err(), Some(FastbootError::ProtocolError));
        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_reboot_bootloader() -> Result<()> {
        let (reboot_client, reboot_server) = create_endpoints::<RebootListenerMarker>();
        let mut stream = reboot_server.into_stream()?;
        let (target, proxy) = setup(vec![Reply::Okay("".to_string())]).await;
        try_join!(
            async move {
                // Should only need to wait for the first request.
                if let Some(RebootListenerRequest::OnReboot { control_handle: _ }) =
                    stream.try_next().await?
                {
                    return target.events.push(TargetEvent::Rediscovered);
                }
                bail!("did not receive reboot event");
            },
            proxy
                .reboot_bootloader(reboot_client)
                .map_err(|e| anyhow!("error rebooting to bootloader: {:?}", e)),
        )
        .and_then(|(_, reboot)| {
            reboot.map_err(|e| anyhow!("failed booting to bootloader: {:?}", e))
        })
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_reboot_bootloader_sends_protocol_error_after_unexpected_reply() -> Result<()> {
        let (reboot_client, _) = create_endpoints::<RebootListenerMarker>();
        let (_, proxy) = setup(vec![Reply::Fail("".to_string())]).await;
        let res = proxy.reboot_bootloader(reboot_client).await?;
        assert_eq!(res.err(), Some(RebootError::FastbootError));
        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_reboot_bootloader_sends_communication_error_if_reboot_listener_dropped(
    ) -> Result<()> {
        let (reboot_client, _) = create_endpoints::<RebootListenerMarker>();
        let (_, proxy) = setup(vec![Reply::Okay("".to_string())]).await;
        let res = proxy.reboot_bootloader(reboot_client).await?;
        assert_eq!(res.err(), Some(RebootError::FailedToSendOnReboot));
        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    // Disabling until we can make this not rely on a timeout
    #[ignore]
    async fn test_reboot_bootloader_sends_rediscovered_error_if_not_rediscovered() -> Result<()> {
        let (reboot_client, reboot_server) = create_endpoints::<RebootListenerMarker>();
        let mut stream = reboot_server.into_stream()?;
        let (_, proxy) = setup(vec![Reply::Okay("".to_string())]).await;
        try_join!(
            async move {
                // Should only need to wait for the first request.
                if let Some(RebootListenerRequest::OnReboot { control_handle: _ }) =
                    stream.try_next().await?
                {
                    // Don't push rediscovered event
                    return Ok(());
                }
                bail!("did not receive reboot event");
            },
            proxy
                .reboot_bootloader(reboot_client)
                .map_err(|e| anyhow!("error rebooting to bootloader: {:?}", e)),
        )
        .and_then(|(_, reboot)| {
            assert_eq!(reboot.err(), Some(RebootError::FailedToSendOnReboot));
            Ok(())
        })
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_continue_boot() -> Result<()> {
        let (_, proxy) = setup(vec![Reply::Okay("".to_string())]).await;
        proxy.continue_boot().await?.map_err(|e| anyhow!("error continue boot: {:?}", e))
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_continue_boot_sends_protocol_error_after_unexpected_reply() -> Result<()> {
        let (_, proxy) = setup(vec![Reply::Fail("".to_string())]).await;
        let res = proxy.continue_boot().await?;
        assert_eq!(res.err(), Some(FastbootError::ProtocolError));
        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_set_active() -> Result<()> {
        let (_, proxy) = setup(vec![Reply::Okay("".to_string())]).await;
        proxy.set_active("a").await?.map_err(|e| anyhow!("error set active: {:?}", e))
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_set_active_sends_protocol_error_after_unexpected_reply() -> Result<()> {
        let (_, proxy) = setup(vec![Reply::Fail("".to_string())]).await;
        let res = proxy.set_active("a").await?;
        assert_eq!(res.err(), Some(FastbootError::ProtocolError));
        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_stage() -> Result<()> {
        let (prog_client, prog_server) = create_endpoints::<UploadProgressListenerMarker>();
        let mut stream = prog_server.into_stream()?;
        let file: NamedTempFile = NamedTempFile::new().expect("tmp access failed");
        let mut buffer = BufWriter::new(&file);
        buffer.write_all(b"Test")?;
        buffer.flush()?;
        let (_, proxy) = setup(vec![
            Reply::Data(4),
            Reply::Okay("".to_string()), //Download Reply
        ])
        .await;
        let filepath = file.path().to_str().ok_or(anyhow!("error getting tempfile path"))?;
        try_join!(
            async move {
                while let Some(_) = stream.try_next().await? { /* do nothing */ }
                Ok(())
            },
            proxy.stage(filepath, prog_client).map_err(|e| anyhow!("error staging: {:?}", e)),
        )
        .and_then(|(_, stage)| {
            assert!(stage.is_ok());
            Ok(())
        })
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_stage_sends_protocol_error_after_unexpected_reply() -> Result<()> {
        let (prog_client, prog_server) = create_endpoints::<UploadProgressListenerMarker>();
        let mut stream = prog_server.into_stream()?;
        let file: NamedTempFile = NamedTempFile::new().expect("tmp access failed");
        let mut buffer = BufWriter::new(&file);
        buffer.write_all(b"Test")?;
        buffer.flush()?;
        let (_, proxy) = setup(vec![Reply::Data(6)]).await;
        let filepath = file.path().to_str().ok_or(anyhow!("error getting tempfile path"))?;
        try_join!(
            async move {
                while let Some(_) = stream.try_next().await? { /* do nothing */ }
                Ok(())
            },
            proxy.stage(filepath, prog_client).map_err(|e| anyhow!("error staging: {:?}", e)),
        )
        .and_then(|(_, stage)| {
            assert_eq!(stage.err(), Some(FastbootError::ProtocolError));
            Ok(())
        })
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_oem() -> Result<()> {
        let (_, proxy) = setup(vec![Reply::Okay("".to_string())]).await;
        proxy.oem("a").await?.map_err(|e| anyhow!("error oem: {:?}", e))
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_oem_sends_protocol_error_after_unexpected_reply() -> Result<()> {
        let (_, proxy) = setup(vec![Reply::Fail("".to_string())]).await;
        let res = proxy.oem("a").await?;
        assert_eq!(res.err(), Some(FastbootError::ProtocolError));
        Ok(())
    }

    // The following four tests interact due to the SEND_LOCK in fastboot, so they
    // are marked as "serial"

    #[fuchsia::test]
    #[serial]
    async fn test_timeout_on_boot_is_ok() -> Result<()> {
        let (_, proxy) = setup_with_timeout(vec![], true).await;
        proxy
            .boot()
            .map_err(|e| anyhow!("FIDL failure: {e:?}"))
            .on_timeout(Duration::from_secs(5), || Err(anyhow!("boot test timed out")))
            .await?
            .map_err(|e| anyhow!("fastboot boot failed: {e:?}"))
    }

    #[fuchsia::test]
    #[serial]
    async fn test_timeout_on_reboot_is_ok() -> Result<()> {
        let mut interface = TestTransport::new(true);
        reboot(&mut interface)
            .on_timeout(Duration::from_secs(5), || Err(anyhow!("reboot test timed out")))
            .await
    }

    #[fuchsia::test]
    #[serial]
    async fn test_timeout_on_reboot_bootloader_is_ok() -> Result<()> {
        let mut interface = TestTransport::new(true);
        reboot_bootloader(&mut interface)
            .on_timeout(Duration::from_secs(5), || Err(anyhow!("rbb test timed out")))
            .await
    }

    #[fuchsia::test]
    #[serial]
    async fn test_timeout_on_continue_is_ok() -> Result<()> {
        let mut interface = TestTransport::new(true);
        continue_boot(&mut interface)
            .on_timeout(Duration::from_secs(5), || Err(anyhow!("continue_boot test timed out")))
            .await
    }
}
