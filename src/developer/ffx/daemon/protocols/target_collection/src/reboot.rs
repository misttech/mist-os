// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use addr::TargetAddr;
use anyhow::{anyhow, bail, Result};
use async_utils::async_once::Once;
use ffx_daemon_events::TargetConnectionState;
use ffx_daemon_target::{
    fastboot::Fastboot,
    target::Target,
    zedboot::{reboot, reboot_to_bootloader, reboot_to_recovery},
};
use ffx_ssh::ssh::get_ssh_key_paths;
use fidl::{
    endpoints::{DiscoverableProtocolMarker as _, ServerEnd},
    Error,
};
use fidl_fuchsia_developer_ffx::{
    self as ffx, RebootListenerRequest, TargetRebootError, TargetRebootResponder, TargetRebootState,
};
use fidl_fuchsia_developer_remotecontrol::RemoteControlProxy;
use fidl_fuchsia_hardware_power_statecontrol::{AdminMarker, AdminProxy, RebootReason};
use fidl_fuchsia_io::OpenFlags;
use futures::{try_join, TryFutureExt, TryStreamExt};
use std::net::IpAddr;
use std::process::Command;
use std::rc::Rc;
use std::rc::Weak;
use tasks::TaskManager;

// TODO(125639): Remove when Power Manager stabilizes
/// Configuration flag which enables using `dm` over ssh to reboot the target
/// when it is in product mode
const USE_SSH_FOR_REBOOT_FROM_PRODUCT: &'static str = "product.reboot.use_dm";

const ADMIN_MONIKER: &'static str = "/bootstrap/shutdown_shim";

pub(crate) struct RebootController {
    target: Rc<Target>,
    remote_proxy: Once<RemoteControlProxy>,
    fastboot_proxy: Once<ffx::FastbootProxy>,
    admin_proxy: Once<AdminProxy>,
    tasks: TaskManager,
}

impl RebootController {
    pub(crate) fn new(target: Rc<Target>) -> Self {
        Self {
            target,
            remote_proxy: Once::new(),
            fastboot_proxy: Once::new(),
            admin_proxy: Once::new(),
            tasks: Default::default(),
        }
    }

    async fn get_remote_proxy(&self) -> Result<RemoteControlProxy> {
        // TODO(awdavies): Factor out init_remote_proxy from the target, OR
        // move the impl(s) here that rely on remote control to use init_remote_proxy
        // instead.
        self.remote_proxy
            .get_or_try_init(self.target.init_remote_proxy())
            .await
            .map(|proxy| proxy.clone())
    }

    async fn get_fastboot_proxy(&self) -> Result<ffx::FastbootProxy> {
        self.fastboot_proxy.get_or_try_init(self.fastboot_init()).await.map(|p| p.clone())
    }

    async fn get_admin_proxy(&self) -> Result<AdminProxy> {
        self.admin_proxy.get_or_try_init(self.init_admin_proxy()).await.map(|p| p.clone())
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

    async fn fastboot_init(&self) -> Result<ffx::FastbootProxy> {
        let (proxy, fastboot) = fidl::endpoints::create_proxy::<ffx::FastbootMarker>()?;
        self.spawn_fastboot(fastboot).await?;
        Ok(proxy)
    }

    #[tracing::instrument(skip(self))]
    pub(crate) async fn spawn_fastboot(
        &self,
        fastboot: ServerEnd<ffx::FastbootMarker>,
    ) -> Result<()> {
        let mut fastboot_manager = Fastboot::new(self.target.clone());
        let stream = fastboot.into_stream()?;
        self.tasks.spawn(async move {
            match fastboot_manager.handle_fastboot_requests_from_stream(stream).await {
                Ok(_) => tracing::trace!("Fastboot proxy finished - client disconnected"),
                Err(e) => tracing::error!("Handling fastboot requests: {:?}", e),
            }
        });
        Ok(())
    }

    #[tracing::instrument(skip(self))]
    pub(crate) async fn reboot(
        &self,
        state: TargetRebootState,
        responder: TargetRebootResponder,
    ) -> Result<()> {
        match self.target.get_connection_state() {
            TargetConnectionState::Fastboot(_) => match state {
                TargetRebootState::Product => {
                    match self.get_fastboot_proxy().await?.reboot().await? {
                        Ok(_) => responder.send(Ok(())).map_err(Into::into),
                        Err(e) => {
                            tracing::error!("Fastboot communication error: {:?}", e);
                            responder
                                .send(Err(TargetRebootError::FastbootCommunication))
                                .map_err(Into::into)
                        }
                    }
                }
                TargetRebootState::Bootloader => {
                    let (reboot_client, reboot_server) =
                        fidl::endpoints::create_endpoints::<ffx::RebootListenerMarker>();
                    let mut stream = reboot_server.into_stream()?;
                    match try_join!(
                        self.get_fastboot_proxy().await?.reboot_bootloader(reboot_client).map_err(
                            |e| anyhow!("fidl error when rebooting to bootloader: {:?}", e)
                        ),
                        async move {
                            if let Some(RebootListenerRequest::OnReboot { control_handle: _ }) =
                                stream.try_next().await?
                            {
                                Ok(())
                            } else {
                                bail!("Did not receive reboot signal");
                            }
                        }
                    ) {
                        Ok(_) => responder.send(Ok(())).map_err(Into::into),
                        Err(e) => {
                            tracing::error!("Fastboot communication error: {:?}", e);
                            responder
                                .send(Err(TargetRebootError::FastbootCommunication))
                                .map_err(Into::into)
                        }
                    }
                }
                TargetRebootState::Recovery => {
                    responder.send(Err(TargetRebootError::FastbootToRecovery)).map_err(Into::into)
                }
            },
            TargetConnectionState::Zedboot(_) => {
                let response = if let Some(addr) = self.target.netsvc_address() {
                    match state {
                        TargetRebootState::Product => reboot(addr).await.map(|_| ()).map_err(|e| {
                            tracing::error!("zedboot reboot failed {:?}", e);
                            TargetRebootError::NetsvcCommunication
                        }),
                        TargetRebootState::Bootloader => {
                            reboot_to_bootloader(addr).await.map(|_| ()).map_err(|e| {
                                tracing::error!("zedboot reboot to bootloader failed {:?}", e);
                                TargetRebootError::NetsvcCommunication
                            })
                        }
                        TargetRebootState::Recovery => {
                            reboot_to_recovery(addr).await.map(|_| ()).map_err(|e| {
                                tracing::error!("zedboot reboot to recovery failed {:?}", e);
                                TargetRebootError::NetsvcCommunication
                            })
                        }
                    }
                } else {
                    Err(TargetRebootError::NetsvcAddressNotFound)
                };
                responder.send(response).map_err(Into::into)
            }
            // Everything else use AdminProxy
            _ => {
                //TODO(125639): Remove when Power Manager stabilizes
                let use_ssh_for_reboot: bool =
                    ffx_config::get(USE_SSH_FOR_REBOOT_FROM_PRODUCT).await.unwrap_or(false);

                if use_ssh_for_reboot {
                    let res = run_ssh_command(Rc::downgrade(&self.target), state).await;
                    match res {
                        Ok(_) => responder.send(Ok(())).map_err(Into::into),
                        Err(e) => {
                            tracing::error!("Target communication error when rebooting: {:?}", e);
                            responder
                                .send(Err(TargetRebootError::TargetCommunication))
                                .map_err(Into::into)
                        }
                    }
                } else {
                    let admin_proxy = match self
                        .get_admin_proxy()
                        .map_err(|e| {
                            tracing::warn!("error getting admin proxy: {}", e);
                            TargetRebootError::TargetCommunication
                        })
                        .await
                    {
                        Ok(a) => a,
                        Err(e) => {
                            responder.send(Err(e))?;
                            return Err(anyhow!("failed to get admin proxy"));
                        }
                    };
                    match state {
                        TargetRebootState::Product => {
                            match admin_proxy.reboot(RebootReason::UserRequest).await {
                                Ok(_) => responder.send(Ok(())).map_err(Into::into),
                                Err(e) => {
                                    handle_fidl_connection_err(e, responder).map_err(Into::into)
                                }
                            }
                        }
                        TargetRebootState::Bootloader => {
                            match admin_proxy.reboot_to_bootloader().await {
                                Ok(_) => responder.send(Ok(())).map_err(Into::into),
                                Err(e) => {
                                    handle_fidl_connection_err(e, responder).map_err(Into::into)
                                }
                            }
                        }
                        TargetRebootState::Recovery => match admin_proxy.reboot_to_recovery().await
                        {
                            Ok(_) => responder.send(Ok(())).map_err(Into::into),
                            Err(e) => handle_fidl_connection_err(e, responder).map_err(Into::into),
                        },
                    }
                }
            }
        }
    }
}

#[tracing::instrument]
pub(crate) fn handle_fidl_connection_err(e: Error, responder: TargetRebootResponder) -> Result<()> {
    match e {
        Error::ClientChannelClosed { .. } => {
            tracing::warn!(
                "Reboot returned a client channel closed - assuming reboot succeeded: {:?}",
                e
            );
            responder.send(Ok(()))?;
        }
        _ => {
            tracing::error!("Target communication error: {:?}", e);
            responder.send(Err(TargetRebootError::TargetCommunication))?;
        }
    }
    Ok(())
}

// TODO(125639): Remove this block

static DEFAULT_SSH_OPTIONS: &'static [&str] = &[
    // We do not want multiplexing
    "-o",
    "ControlPath none",
    "-o",
    "ControlMaster no",
    "-o",
    "ExitOnForwardFailure yes",
    "-o",
    "StreamLocalBindUnlink yes",
    "-o",
    "CheckHostIP=no",
    "-o",
    "StrictHostKeyChecking=no",
    "-o",
    "UserKnownHostsFile=/dev/null",
    "-o",
    "LogLevel=ERROR",
];

#[tracing::instrument]
async fn run_ssh_command(target: Weak<Target>, state: TargetRebootState) -> Result<()> {
    let t = target.upgrade().ok_or(anyhow!("Could not upgrade Target to build ssh command"))?;
    let addr = t.ssh_address().ok_or(anyhow!("Could not get ssh address for target"))?;
    let keys = get_ssh_key_paths().await?;
    let mut cmd = build_ssh_command(addr.into(), keys, state);
    tracing::debug!("About to run command on target to reboot: {:?}", cmd);
    let ssh = cmd.spawn()?;
    let output = ssh.wait_with_output()?;
    match output.status.success() {
        true => Ok(()),
        _ => {
            // Exit code 255 indicates that the ssh connection was suddenly dropped
            // assume this is correct behaviour and return
            if let Some(255) = output.status.code() {
                Ok(())
            } else {
                let stdout = output.stdout;
                tracing::error!(
                    "Error rebooting. Error code: {:?}. Output from ssh command: {:?}",
                    output.status.code(),
                    stdout
                );
                Err(anyhow!("Error using `dm` command to reboot to bootloader. Check Daemon Logs"))
            }
        }
    }
}

#[tracing::instrument]
fn build_ssh_command(
    addr: TargetAddr,
    keys: Vec<String>,
    desired_state: TargetRebootState,
) -> Command {
    let mut ssh_cmd = Command::new("ssh");

    for arg in DEFAULT_SSH_OPTIONS {
        ssh_cmd.arg(arg);
    }

    match addr.ip() {
        IpAddr::V4(_) => {
            ssh_cmd.arg("-o");
            ssh_cmd.arg("AddressFamily inet");
        }
        IpAddr::V6(_) => {
            ssh_cmd.arg("-o");
            ssh_cmd.arg("AddressFamily inet6");
        }
    }

    for k in keys {
        ssh_cmd.arg("-i").arg(k);
    }

    // Port and host
    ssh_cmd.arg("-p").arg(format!("{}", addr.port())).arg(format!("{}", addr));

    let device_command = match desired_state {
        TargetRebootState::Bootloader => "dm reboot-bootloader",
        TargetRebootState::Recovery => "dm reboot-recovery",
        TargetRebootState::Product => "dm reboot",
    };

    ssh_cmd.arg(device_command);

    ssh_cmd
}

// END BLOCK

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::anyhow;
    use fidl::endpoints::{create_proxy_and_stream, RequestStream};
    use fidl_fuchsia_developer_ffx::{
        FastbootMarker, FastbootProxy, FastbootRequest, TargetMarker, TargetProxy, TargetRequest,
    };
    use fidl_fuchsia_developer_remotecontrol::{RemoteControlMarker, RemoteControlRequest};
    use fidl_fuchsia_hardware_power_statecontrol::{AdminRequest, AdminRequestStream};
    use std::time::Instant;

    async fn setup_fastboot() -> FastbootProxy {
        let (proxy, mut stream) =
            fidl::endpoints::create_proxy_and_stream::<FastbootMarker>().unwrap();
        fuchsia_async::Task::local(async move {
            while let Ok(Some(req)) = stream.try_next().await {
                match req {
                    FastbootRequest::Reboot { responder } => {
                        responder.send(Ok(())).unwrap();
                    }
                    FastbootRequest::RebootBootloader { listener, responder } => {
                        listener.into_proxy().unwrap().on_reboot().unwrap();
                        responder.send(Ok(())).unwrap();
                    }
                    _ => assert!(false),
                }
            }
        })
        .detach();
        proxy
    }

    fn setup_admin(chan: fidl::Channel) -> Result<()> {
        let mut stream = AdminRequestStream::from_channel(fidl::AsyncChannel::from_channel(chan)?);
        fuchsia_async::Task::local(async move {
            while let Ok(Some(req)) = stream.try_next().await {
                match req {
                    AdminRequest::Reboot { reason: RebootReason::UserRequest, responder } => {
                        responder.send(Ok(())).unwrap();
                    }
                    AdminRequest::RebootToBootloader { responder } => {
                        responder.send(Ok(())).unwrap();
                    }
                    AdminRequest::RebootToRecovery { responder } => {
                        responder.send(Ok(())).unwrap();
                    }
                    _ => assert!(false),
                }
            }
        })
        .detach();
        Ok(())
    }

    async fn setup_remote() -> RemoteControlProxy {
        let (proxy, mut stream) =
            fidl::endpoints::create_proxy_and_stream::<RemoteControlMarker>().unwrap();
        fuchsia_async::Task::local(async move {
            while let Ok(Some(req)) = stream.try_next().await {
                match req {
                    RemoteControlRequest::ConnectCapability { server_chan, responder, .. } => {
                        setup_admin(server_chan).unwrap();
                        responder.send(Ok(())).unwrap();
                    }
                    _ => assert!(false),
                }
            }
        })
        .detach();
        proxy
    }

    async fn setup() -> (Rc<Target>, TargetProxy) {
        let target = Target::new_named("scooby-dooby-doo");
        let fastboot_proxy = Once::new();
        let _ = fastboot_proxy.get_or_init(setup_fastboot()).await;
        let remote_proxy = Once::new();
        let _ = remote_proxy.get_or_init(setup_remote()).await;
        let admin_proxy = Once::new();
        let rc = RebootController {
            target: target.clone(),
            fastboot_proxy,
            remote_proxy,
            admin_proxy,
            tasks: Default::default(),
        };
        let (proxy, mut stream) = create_proxy_and_stream::<TargetMarker>().unwrap();
        fuchsia_async::Task::local(async move {
            while let Ok(Some(req)) = stream.try_next().await {
                match req {
                    TargetRequest::Reboot { state, responder } => {
                        rc.reboot(state, responder).await.unwrap();
                    }
                    r => panic!("received unexpected request {:?}", r),
                }
            }
        })
        .detach();
        (target, proxy)
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_reboot_product() -> Result<()> {
        let (_, proxy) = setup().await;
        proxy
            .reboot(TargetRebootState::Product)
            .await?
            .map_err(|e| anyhow!("error rebooting: {:?}", e))
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_reboot_recovery() -> Result<()> {
        let (_, proxy) = setup().await;
        proxy
            .reboot(TargetRebootState::Recovery)
            .await?
            .map_err(|e| anyhow!("error rebooting: {:?}", e))
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_reboot_bootloader() -> Result<()> {
        let (_, proxy) = setup().await;
        proxy
            .reboot(TargetRebootState::Bootloader)
            .await?
            .map_err(|e| anyhow!("error rebooting: {:?}", e))
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_fastboot_reboot_product() -> Result<()> {
        let (target, proxy) = setup().await;
        target.set_state(TargetConnectionState::Fastboot(Instant::now()));
        proxy
            .reboot(TargetRebootState::Product)
            .await?
            .map_err(|e| anyhow!("error rebooting: {:?}", e))
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_fastboot_reboot_recovery() -> Result<()> {
        let (target, proxy) = setup().await;
        target.set_state(TargetConnectionState::Fastboot(Instant::now()));
        assert!(proxy.reboot(TargetRebootState::Recovery).await?.is_err());
        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_fastboot_reboot_bootloader() -> Result<()> {
        let (target, proxy) = setup().await;
        target.set_state(TargetConnectionState::Fastboot(Instant::now()));
        proxy
            .reboot(TargetRebootState::Bootloader)
            .await?
            .map_err(|e| anyhow!("error rebooting: {:?}", e))
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_zedboot_reboot_bootloader() -> Result<()> {
        let (target, proxy) = setup().await;
        target.set_state(TargetConnectionState::Zedboot(Instant::now()));
        assert!(proxy.reboot(TargetRebootState::Bootloader).await?.is_err());
        Ok(())
    }

    #[fuchsia_async::run_singlethreaded(test)]
    async fn test_zedboot_reboot_recovery() -> Result<()> {
        let (target, proxy) = setup().await;
        target.set_state(TargetConnectionState::Zedboot(Instant::now()));
        assert!(proxy.reboot(TargetRebootState::Recovery).await?.is_err());
        Ok(())
    }
}
