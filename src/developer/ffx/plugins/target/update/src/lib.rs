// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use ffx_config::EnvironmentContext;
use ffx_update_args::ForceInstall;
use fho::{
    bug, deferred, return_user_error, Deferred, FfxContext, FfxMain, FfxTool, Result, SimpleWriter,
};
use fidl_fuchsia_update::{
    CheckOptions, Initiator, ManagerProxy, MonitorMarker, MonitorRequest, MonitorRequestStream,
};
use fidl_fuchsia_update_channelcontrol::ChannelControlProxy;
use fidl_fuchsia_update_ext::State;
use fidl_fuchsia_update_installer::{self as finstaller, InstallerProxy};
use fuchsia_async::Timer;
use fuchsia_url::AbsolutePackageUrl;
use futures::future::FutureExt as _;
use futures::{pin_mut, select, TryStreamExt};
use std::path::PathBuf;
use std::time::Duration;
use target_connector::Connector;
use target_holders::{daemon_protocol, moniker, RemoteControlProxyHolder, TargetProxyHolder};
use {
    ffx_update_args as args, fidl_fuchsia_developer_ffx as ffx,
    fidl_fuchsia_update_installer_ext as installer,
};

mod server;

#[derive(FfxTool)]
pub struct UpdateTool {
    #[command]
    cmd: args::Update,
    context: EnvironmentContext,
    #[with(moniker("/core/system-update"))]
    update_manager_proxy: ManagerProxy,
    #[with(moniker("/core/system-update"))]
    channel_control_proxy: ChannelControlProxy,
    #[with(deferred(moniker("/core/system-update/system-updater")))]
    installer_proxy: Deferred<InstallerProxy>,
    target_proxy_connector: Connector<TargetProxyHolder>,
    rcs_proxy_connector: Connector<RemoteControlProxyHolder>,
    #[with(deferred(daemon_protocol()))]
    repos: Deferred<ffx::RepositoryRegistryProxy>,
}

fho::embedded_plugin!(UpdateTool);

#[async_trait(?Send)]
impl FfxMain for UpdateTool {
    type Writer = SimpleWriter;

    /// Main entry point for the `update` subcommand.
    async fn main(self, mut writer: Self::Writer) -> Result<()> {
        let update = self.cmd.clone();

        match update.cmd {
            args::Command::Channel(args::Channel { ref cmd }) => {
                handle_channel_control_cmd(&cmd, self.channel_control_proxy, &mut writer).await?;
            }
            args::Command::CheckNow(ref check_now) => {
                Box::pin(self.handle_check_now_cmd(check_now, &mut writer)).await?;
            }
            args::Command::ForceInstall(ref args) => {
                Box::pin(self.handle_force_install_cmd(args, &mut writer)).await?;
            }
        }
        Ok(())
    }
}

impl UpdateTool {
    /// If there's a new version available, update to it, printing progress to the
    /// console during the process.
    async fn handle_check_now_cmd<W: std::io::Write>(
        self,
        cmd: &args::CheckNow,
        writer: &mut W,
    ) -> Result<()> {
        let package_server_task = if cmd.product_bundle {
            let product_path =
                Self::get_product_bundle_path(&cmd.product_bundle_path, &self.context.clone())?;
            let repo_port: u16 = cmd.product_bundle_port()?.try_into().unwrap();
            server::package_server_task(
                self.target_proxy_connector.clone(),
                self.rcs_proxy_connector.clone(),
                self.repos,
                self.context.clone(),
                product_path,
                repo_port,
            )
            .await?
        } else if cmd.product_bundle_path.is_some() {
            return_user_error!(
                "Cannot specify a product bundle without the the `--product-bundle option."
            );
        } else {
            None
        };

        if let Some(server_task) = package_server_task {
            // Use select! to run the package server at the same time as the others. This is preferable
            // to using detach(), since we can get error result from the package server.

            // wait for the server to be registered before running the check.
            let check = async {
                server::wait_for_device_task(
                    server_task.repo_name.clone(),
                    self.rcs_proxy_connector.clone(),
                )
                .await?;
                Self::check_for_update(self.update_manager_proxy.clone(), &cmd, writer).await
            };

            let fused_server_task = server_task.task.fuse();
            let check_task = check.fuse();

            pin_mut!(fused_server_task, check_task);

            let repo_name = server_task.repo_name.clone();

            select!(
                server_task_result = fused_server_task =>  {
                    // The server should start and run indefiniitely, if we get here there is a problem.
                    match server_task_result {
                    Ok(_) => return_user_error!("Package server exited successfully, but prematurely"),
                    Err(e) => return_user_error!("Package server failed to run: {e}")
                    }
                }
                update_task_result =  check_task => {
                      server::unregister_pb_repo_server(&repo_name, self.rcs_proxy_connector.clone()).await?;
                   return update_task_result}
            );
        } else {
            Self::check_for_update(self.update_manager_proxy.clone(), &cmd, writer).await?;
        }

        Ok(())
    }

    async fn check_for_update<W: std::io::Write>(
        update_manager_proxy: ManagerProxy,
        cmd: &args::CheckNow,
        writer: &mut W,
    ) -> Result<()> {
        let do_monitor = cmd.monitor || cmd.product_bundle;
        let options = CheckOptions {
            initiator: Some(if cmd.service_initiated {
                Initiator::Service
            } else {
                Initiator::User
            }),
            allow_attaching_to_existing_update_check: Some(true),
            ..Default::default()
        };

        // Create the monitor client if requested, or if using a product bundle as the source.
        // This is needed for product bundles to make sure the package server continues to run
        // until the update is completed.
        let (monitor_client, monitor_server) = if do_monitor {
            let (client_end, request_stream) =
                fidl::endpoints::create_request_stream::<MonitorMarker>();
            (Some(client_end), Some(request_stream))
        } else {
            (None, None)
        };

        match update_manager_proxy.check_now(&options, monitor_client).await {
            Ok(ok_result) => {
                if let Err(e) = ok_result {
                    return_user_error!("Not started error on check-now: {e:?}")
                }
            }
            Err(e) => return_user_error!("Error on check-now: {e:?}"),
        };
        writeln!(writer, "Checking for an update.").map_err(|e| bug!(e))?;
        if let Some(monitor_server) = monitor_server {
            monitor_state(monitor_server, writer).await?;
        }
        Ok(())
    }

    /// Change to a specific version, regardless of whether it's newer or older than
    /// the current system software.
    async fn handle_force_install_cmd<W: std::io::Write>(
        self,
        cmd: &ForceInstall,
        writer: &mut W,
    ) -> Result<()> {
        let package_server_task = if cmd.product_bundle {
            let product_path =
                Self::get_product_bundle_path(&cmd.product_bundle_path, &self.context.clone())?;

            let repo_port: u16 = cmd.product_bundle_port()?.try_into().unwrap();
            server::package_server_task(
                self.target_proxy_connector.clone(),
                self.rcs_proxy_connector.clone(),
                self.repos,
                self.context.clone(),
                product_path,
                repo_port,
            )
            .await?
        } else if cmd.product_bundle_path.is_some() {
            return_user_error!(
                "Cannot specify a product bundle without the the `--product-bundle option."
            );
        } else {
            None
        };

        let installer_proxy = self.installer_proxy.await?;

        let pkg_url = AbsolutePackageUrl::parse(&cmd.update_pkg_url)
            .bug_context("parsing update package url")?;

        if let Some(server_task) = package_server_task {
            // Use select! to run the package server at the same time as the others. This is preferable
            // to using detach(), since we can get error result from the package server.

            // wait for the server to be registered before running the check.
            let install = async {
                server::wait_for_device_task(
                    server_task.repo_name.clone(),
                    self.rcs_proxy_connector.clone(),
                )
                .await?;
                Self::force_install(pkg_url, cmd.reboot, installer_proxy, writer).await
            };

            let fused_server_task = server_task.task.fuse();
            let install_task = install.fuse();

            pin_mut!(fused_server_task, install_task);

            let repo_name = server_task.repo_name.clone();

            select!(
                server_task_result = fused_server_task =>  {
                    // The server should start and run indefiniitely, if we get here there is a problem.
                    match server_task_result {
                    Ok(_) => return_user_error!("Package server exited successfully, but prematurely"),
                    Err(e) => return_user_error!("Package server failed to run: {e}")
                    }
                }
                update_task_result =  install_task => {
                    if cmd.reboot {
                        Timer::new(Duration::from_secs(15)).await;
                    }
                      server::unregister_pb_repo_server(&repo_name, self.rcs_proxy_connector.clone()).await?;
                    return update_task_result}
            );
        } else {
            Self::force_install(pkg_url, cmd.reboot, installer_proxy, writer).await
        }
    }

    async fn force_install<W: std::io::Write>(
        pkg_url: AbsolutePackageUrl,
        reboot: bool,
        installer_proxy: InstallerProxy,
        writer: &mut W,
    ) -> Result<()> {
        let options = installer::Options {
            initiator: installer::Initiator::User,
            should_write_recovery: true,
            allow_attach_to_existing_attempt: true,
        };

        let (reboot_controller, reboot_controller_server_end) =
            fidl::endpoints::create_proxy::<finstaller::RebootControllerMarker>();

        let mut update_attempt = installer::start_update(
            &pkg_url,
            options,
            &installer_proxy,
            Some(reboot_controller_server_end),
        )
        .await
        .bug_context("starting update")?;

        writeln!(
            writer,
            "Installing an update.
Progress reporting is based on the fraction of packages resolved, so if one package is much
larger than the others, then the reported progress could appear to stall near the end.
Until the update process is improved to have more granular reporting, try using
    ffx inspect show 'core/pkg-resolver'
for more detail on the progress of update-related downloads.\n"
        )
        .map_err(|e| bug!(e))?;
        if !reboot {
            reboot_controller.detach().bug_context("notify installer do not reboot")?;
        }
        write_progress("\nStarting install", writer)?;
        while let Some(state) = update_attempt.try_next().await.bug_context("getting next state")? {
            match state {
                fidl_fuchsia_update_installer_ext::State::WaitToReboot(info) => {
                    // if waiting for reboot, wait for a while to get a head start, hopefully returning after
                    // the shutdown.
                    write_progress(
                        &format!(
                            "{:.1} {}/{} Waiting to Reboot",
                            info.progress().fraction_completed() * 100.0,
                            info.progress().bytes_downloaded(),
                            info.info().download_size()
                        ),
                        writer,
                    )?;
                    write!(writer, "\n").map_err(|e| bug!(e))?;
                    if reboot {
                        return Ok(());
                    }
                }
                fidl_fuchsia_update_installer_ext::State::Reboot(info)
                | fidl_fuchsia_update_installer_ext::State::DeferReboot(info)
                | fidl_fuchsia_update_installer_ext::State::Complete(info) => {
                    write_progress(
                        &format!(
                            "{:.1} {}/{} Complete",
                            info.progress().fraction_completed() * 100.0,
                            info.progress().bytes_downloaded(),
                            info.info().download_size()
                        ),
                        writer,
                    )?;
                    return Ok(());
                }

                fidl_fuchsia_update_installer_ext::State::FailPrepare(reason) => {
                    return_user_error!("Install failed: {reason:?}")
                }
                fidl_fuchsia_update_installer_ext::State::FailStage(data) => {
                    return_user_error!("Install failed: {:?}", data.reason())
                }
                fidl_fuchsia_update_installer_ext::State::FailFetch(data) => {
                    return_user_error!("Install failed: {:?}", data.reason())
                }
                fidl_fuchsia_update_installer_ext::State::Canceled => {
                    return_user_error!("Install failed: canceled")
                }

                fidl_fuchsia_update_installer_ext::State::Prepare => {
                    write_progress(&format!("{:.1} {}/{} Preparing", 0.0, 0, "?"), writer)?
                }
                fidl_fuchsia_update_installer_ext::State::Stage(info) => write_progress(
                    &format!(
                        "{:.1} {}/{} Staging",
                        info.progress().fraction_completed() * 100.0,
                        info.progress().bytes_downloaded(),
                        info.info().download_size()
                    ),
                    writer,
                )?,
                fidl_fuchsia_update_installer_ext::State::Fetch(info) => write_progress(
                    &format!(
                        "{:.1} {}/{} Fetching",
                        info.progress().fraction_completed() * 100.0,
                        info.progress().bytes_downloaded(),
                        info.info().download_size()
                    ),
                    writer,
                )?,
                fidl_fuchsia_update_installer_ext::State::Commit(info) => write_progress(
                    &format!(
                        "{:.1} {}/{} Commit",
                        info.progress().fraction_completed() * 100.0,
                        info.progress().bytes_downloaded(),
                        info.info().download_size()
                    ),
                    writer,
                )?,
                fidl_fuchsia_update_installer_ext::State::FailCommit(info) => write_progress(
                    &format!(
                        "{:.1} {}/{} Failed commit",
                        info.progress().fraction_completed() * 100.0,
                        info.progress().bytes_downloaded(),
                        info.info().download_size()
                    ),
                    writer,
                )?,
            }
        }

        Ok(())
    }

    fn get_product_bundle_path(
        product_bundle_path: &Option<PathBuf>,
        context: &EnvironmentContext,
    ) -> Result<PathBuf> {
        let pb_path = match product_bundle_path {
            Some(product_path) => product_path.clone(),
            None => {
                if let Some(product_path) =
                    context.get::<Option<PathBuf>, _>("product.path").map_err(|e| bug!(e))?
                {
                    product_path
                } else {
                    return_user_error!("No product bundle path specified nor configured")
                }
            }
        };
        Ok(pb_path)
    }
}

fn write_progress<W: std::io::Write>(s: &str, writer: &mut W) -> Result<()> {
    // Use escape sequences to make this line overwrite the current terminal line.
    // \r: send cursor to start of line
    // \x1b[K: clear to end of line
    if termion::is_tty(&std::io::stdout()) {
        write!(writer, "\r{s}\x1b[K").map_err(|e| bug!(e))?;
    } else {
        writeln!(writer, "{s}").map_err(|e| bug!(e))?;
    }
    writer.flush().map_err(|e| bug!(e))
}

/// Handle subcommands for `update channel`.
async fn handle_channel_control_cmd<W: std::io::Write>(
    cmd: &args::channel::Command,
    channel_control: fidl_fuchsia_update_channelcontrol::ChannelControlProxy,
    writer: &mut W,
) -> Result<()> {
    match cmd {
        args::channel::Command::Get(_) => {
            let channel = channel_control.get_current().await.map_err(|e| bug!(e))?;
            writeln!(writer, "current channel: {}", channel).map_err(|e| bug!(e))?;
        }
        args::channel::Command::Target(_) => {
            let channel = channel_control.get_target().await.map_err(|e| bug!(e))?;
            writeln!(writer, "target channel: {}", channel).map_err(|e| bug!(e))?;
        }
        args::channel::Command::Set(args::channel::Set { channel }) => {
            channel_control.set_target(&channel).await.map_err(|e| bug!(e))?;
        }
        args::channel::Command::List(_) => {
            let channels = channel_control.get_target_list().await.map_err(|e| bug!(e))?;
            if channels.is_empty() {
                writeln!(writer, "known channels list is empty.").map_err(|e| bug!(e))?;
            } else {
                writeln!(writer, "known channels:").map_err(|e| bug!(e))?;
                for channel in channels {
                    writeln!(writer, "{}", channel).map_err(|e| bug!(e))?;
                }
            }
        }
    }
    Ok(())
}

/// Wait for and print state changes. For informational / DX purposes.
async fn monitor_state<W: std::io::Write>(
    mut stream: MonitorRequestStream,
    writer: &mut W,
) -> Result<()> {
    while let Some(event) = stream.try_next().await.map_err(|e| bug!(e))? {
        match event {
            MonitorRequest::OnState { state, responder } => {
                responder.send().map_err(|e| bug!(e))?;

                let state = State::from(state);
                match state.clone() {
                    State::CheckingForUpdates => write_progress("Checking for updates", writer)?,
                    State::ErrorCheckingForUpdate => {
                        write_progress("Error checking for updates", writer)?
                    }
                    State::NoUpdateAvailable => write_progress("No update available", writer)?,
                    State::InstallationDeferredByPolicy(installation_deferred_data) => {
                        let reason =
                            if let Some(reason) = installation_deferred_data.deferral_reason {
                                format!("{reason:?}")
                            } else {
                                "".into()
                            };
                        write_progress(&format!("Update deferred by policy: {reason}"), writer)?
                    }
                    State::InstallingUpdate(installing_data) => {
                        let pct = if let Some(progress) = installing_data.installation_progress {
                            format!("{:.2}", progress.fraction_completed.unwrap_or(0.0) * 100.0)
                        } else {
                            "".into()
                        };
                        write_progress(&format!("{pct} Installing"), writer)?;
                    }
                    State::WaitingForReboot(installing_data) => {
                        let pct = if let Some(progress) = installing_data.installation_progress {
                            format!("{:.2}", progress.fraction_completed.unwrap_or(0.0) * 100.0)
                        } else {
                            "".into()
                        };
                        write_progress(&format!("{pct} Waiting for reboot"), writer)?;
                        Timer::new(Duration::from_secs(15)).await;
                    }
                    State::InstallationError(installing_data) => {
                        let pct = if let Some(progress) = installing_data.installation_progress {
                            format!("{:.2}", progress.fraction_completed.unwrap_or(0.0) * 100.0)
                        } else {
                            "".into()
                        };
                        write_progress(&format!("{pct} Installation error"), writer)?;
                    }
                };
                // Exit if we encounter an error during an update.
                if state.is_error() {
                    return_user_error!("Update failed: {:?}", state)
                }
                if state.is_terminal() {
                    writeln!(writer, "\n").map_err(|e| bug!(e))?;
                    return Ok(());
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;
    use ffx_target::TargetProxy;
    use ffx_update_args::Update;
    use fho::{FhoConnectionBehavior, FhoEnvironment, TestBuffers, TryFromEnv};
    use fidl::endpoints::create_proxy_and_stream;
    use fidl_fuchsia_developer_remotecontrol::RemoteControlProxy;
    use fidl_fuchsia_update::ManagerRequest;
    use fidl_fuchsia_update_channelcontrol::ChannelControlRequest;
    use futures::prelude::*;
    use mock_installer::MockUpdateInstallerService;
    use std::sync::Arc;
    use target_holders::{fake_proxy, FakeInjector};

    async fn perform_channel_control_test<V, O>(
        argument: args::channel::Command,
        verifier: V,
        output: O,
    ) where
        V: Fn(ChannelControlRequest),
        O: Fn(String),
    {
        let (proxy, mut stream) =
            create_proxy_and_stream::<fidl_fuchsia_update_channelcontrol::ChannelControlMarker>();
        let mut buf = Vec::new();
        let fut = async {
            assert_matches!(handle_channel_control_cmd(&argument, proxy, &mut buf).await, Ok(()));
        };
        let stream_fut = async move {
            let result = stream.next().await.unwrap();
            match result {
                Ok(cmd) => verifier(cmd),
                err => panic!("Err in request handler: {:?}", err),
            }
        };
        future::join(fut, stream_fut).await;
        let out = String::from_utf8(buf).unwrap();
        output(out);
    }

    #[fuchsia::test]
    async fn test_channel_get() {
        perform_channel_control_test(
            args::channel::Command::Get(args::channel::Get {}),
            |cmd| match cmd {
                ChannelControlRequest::GetCurrent { responder } => {
                    responder.send("channel").unwrap();
                }
                request => panic!("Unexpected request: {:?}", request),
            },
            |output| assert_eq!(output, "current channel: channel\n"),
        )
        .await;
    }

    #[fuchsia::test]
    async fn test_channel_target() {
        perform_channel_control_test(
            args::channel::Command::Target(args::channel::Target {}),
            |cmd| match cmd {
                ChannelControlRequest::GetTarget { responder } => {
                    responder.send("target-channel").unwrap();
                }
                request => panic!("Unexpected request: {:?}", request),
            },
            |output| assert_eq!(output, "target channel: target-channel\n"),
        )
        .await;
    }

    #[fuchsia::test]
    async fn test_channel_set() {
        perform_channel_control_test(
            args::channel::Command::Set(args::channel::Set { channel: "new-channel".to_string() }),
            |cmd| match cmd {
                ChannelControlRequest::SetTarget { channel, responder } => {
                    assert_eq!(channel, "new-channel");
                    responder.send().unwrap();
                }
                request => panic!("Unexpected request: {:?}", request),
            },
            |output| assert!(output.is_empty()),
        )
        .await;
    }

    #[fuchsia::test]
    async fn test_channel_list_no_channels() {
        perform_channel_control_test(
            args::channel::Command::List(args::channel::List {}),
            |cmd| match cmd {
                ChannelControlRequest::GetTargetList { responder } => {
                    responder.send(&[]).unwrap();
                }
                request => panic!("Unexpected request: {:?}", request),
            },
            |output| assert_eq!(output, "known channels list is empty.\n"),
        )
        .await;
    }

    #[fuchsia::test]
    async fn test_channel_list_with_channels() {
        perform_channel_control_test(
            args::channel::Command::List(args::channel::List {}),
            |cmd| match cmd {
                ChannelControlRequest::GetTargetList { responder } => {
                    responder
                        .send(&["some-channel".to_owned(), "other-channel".to_owned()])
                        .unwrap();
                }
                request => panic!("Unexpected request: {:?}", request),
            },
            |output| assert_eq!(output, "known channels:\nsome-channel\nother-channel\n"),
        )
        .await;
    }

    #[fuchsia::test]
    async fn test_check_now() {
        let test_env = ffx_config::test_init().await.expect("test env");

        let fake_installer_proxy = Deferred::from_output(Ok(fake_proxy(move |req| {
            panic!("Unexpected request: {:?}", req)
        })));
        let fake_channel_control_proxy =
            fake_proxy(move |req| panic!("Unexpected request: {:?}", req));
        let fake_target_proxy: TargetProxy =
            fake_proxy(move |req| panic!("Unexpected request: {:?}", req));
        let fake_rcs_proxy: RemoteControlProxy =
            fake_proxy(move |req| panic!("Unexpected request: {:?}", req));
        let fake_repo_proxy = Deferred::from_output(Ok(fake_proxy(move |req| {
            panic!("Unexpected request: {:?}", req)
        })));
        let fake_update_manager_proxy = fake_proxy(move |req| {
            match req {
                ManagerRequest::CheckNow { responder, .. } => {
                    responder.send(Ok(())).expect("send ok")
                }
                _ => panic!("Unexpected request: {:?}", req),
            };
        });

        let fake_injector = FakeInjector {
            remote_factory_closure: Box::new(move || {
                let value = fake_rcs_proxy.clone();
                Box::pin(async move { Ok(value) })
            }),
            target_factory_closure: Box::new(move || {
                let value = fake_target_proxy.clone();
                Box::pin(async { Ok(value) })
            }),
            ..Default::default()
        };

        let fho_env = FhoEnvironment::new_with_args(&test_env.context, &["some", "test"]);
        fho_env.set_behavior(FhoConnectionBehavior::DaemonConnector(Arc::new(fake_injector))).await;

        let tool = UpdateTool {
            cmd: Update {
                cmd: args::Command::CheckNow(args::CheckNow {
                    service_initiated: false,
                    monitor: true,
                    product_bundle: false,
                    product_bundle_path: None,
                    product_bundle_port: None,
                }),
            },
            context: test_env.context.clone(),
            update_manager_proxy: fake_update_manager_proxy,
            channel_control_proxy: fake_channel_control_proxy,
            installer_proxy: fake_installer_proxy,
            target_proxy_connector: Connector::try_from_env(&fho_env)
                .await
                .expect("Could not make target proxy test connector"),
            rcs_proxy_connector: Connector::try_from_env(&fho_env)
                .await
                .expect("Could not make RCS test connector"),
            repos: fake_repo_proxy,
        };
        let buffers = TestBuffers::default();
        let writer = SimpleWriter::new_test(&buffers);

        let result = tool.main(writer).await;

        let (stdout, stderr) = buffers.into_strings();

        assert!(result.is_ok(), "Expected Ok got {result:?}");
        assert_eq!(stdout, "Checking for an update.\n");
        assert_eq!(stderr, "");
    }

    #[fuchsia::test]
    async fn test_force_install() {
        let test_env = ffx_config::test_init().await.expect("test env");
        let update_info = installer::UpdateInfo::builder().download_size(1000).build();
        let mock_installer = Arc::new(MockUpdateInstallerService::with_states(vec![
            installer::State::Prepare,
            installer::State::Fetch(
                installer::UpdateInfoAndProgress::new(update_info, installer::Progress::none())
                    .unwrap(),
            ),
            installer::State::Stage(
                installer::UpdateInfoAndProgress::new(
                    update_info,
                    installer::Progress::builder()
                        .fraction_completed(0.5)
                        .bytes_downloaded(500)
                        .build(),
                )
                .unwrap(),
            ),
            installer::State::WaitToReboot(installer::UpdateInfoAndProgress::done(update_info)),
        ]));
        let fake_installer_proxy = mock_installer.spawn_installer_service();

        let args = ForceInstall {
            reboot: true,
            update_pkg_url: "fuchsia-pkg://fuchsia.test/update".into(),
            product_bundle: false,
            product_bundle_path: None,
            product_bundle_port: None,
        };

        let fake_update_manager_proxy =
            fake_proxy(move |req| panic!("Unexpected request: {:?}", req));
        let fake_channel_control_proxy =
            fake_proxy(move |req| panic!("Unexpected request: {:?}", req));
        let fake_target_proxy: TargetProxy =
            fake_proxy(move |req| panic!("Unexpected request: {:?}", req));
        let fake_rcs_proxy: RemoteControlProxy =
            fake_proxy(move |req| panic!("Unexpected request: {:?}", req));
        let fake_repo_proxy = Deferred::from_output(Ok(fake_proxy(move |req| {
            panic!("Unexpected request: {:?}", req)
        })));

        let fake_injector = FakeInjector {
            remote_factory_closure: Box::new(move || {
                let value = fake_rcs_proxy.clone();
                Box::pin(async move { Ok(value) })
            }),
            target_factory_closure: Box::new(move || {
                let value = fake_target_proxy.clone();
                Box::pin(async { Ok(value) })
            }),
            ..Default::default()
        };

        let fho_env = FhoEnvironment::new_with_args(&test_env.context, &["some", "test"]);
        fho_env.set_behavior(FhoConnectionBehavior::DaemonConnector(Arc::new(fake_injector))).await;

        let tool = UpdateTool {
            cmd: Update { cmd: args::Command::ForceInstall(args.clone()) },
            context: test_env.context.clone(),
            update_manager_proxy: fake_update_manager_proxy,
            channel_control_proxy: fake_channel_control_proxy,
            installer_proxy: Deferred::from_output(Ok(fake_installer_proxy)),
            target_proxy_connector: Connector::try_from_env(&fho_env)
                .await
                .expect("Could not make target proxy test connector"),
            rcs_proxy_connector: Connector::try_from_env(&fho_env)
                .await
                .expect("Could not make RCS test connector"),
            repos: fake_repo_proxy,
        };

        let buffers = TestBuffers::default();
        let writer = SimpleWriter::new_test(&buffers);
        tool.main(writer).await.expect("success");

        let (stdout, stderr) = buffers.into_strings();

        assert_eq!(stderr, "");
        assert_eq!(stdout, "Installing an update.\n\
        Progress reporting is based on the fraction of packages resolved, so if one package is much\n\
        larger than the others, then the reported progress could appear to stall near the end.\n\
        Until the update process is improved to have more granular reporting, try using\
        \n    ffx inspect show 'core/pkg-resolver'\n\
            for more detail on the progress of update-related downloads.\n\n\n\
            Starting install\n0.0 0/? Preparing\n0.0 0/1000 Fetching\n\
            50.0 500/1000 Staging\n100.0 1000/1000 Waiting to Reboot\n\n");
    }
}
