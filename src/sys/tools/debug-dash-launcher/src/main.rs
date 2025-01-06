// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Context;
use debug_dash_launcher_config::Config;
use fidl::endpoints::{ControlHandle, Responder};
use fuchsia_component::server::ServiceFs;
use fuchsia_inspect::component;
use fuchsia_inspect::health::Reporter;
use futures::prelude::*;
use log::*;
use {fidl_fuchsia_dash as fdash, fuchsia_async as fasync};

mod launch;
mod layout;
mod package_resolver;
mod socket;
mod trampoline;

enum IncomingRequest {
    Launcher(fdash::LauncherRequestStream),
}

#[fuchsia::main]
async fn main() -> Result<(), anyhow::Error> {
    let mut service_fs = ServiceFs::new_local();

    // Initialize inspect.
    component::health().set_starting_up();

    let config = Config::take_from_startup_handle();
    let tools_pkg_url = config.tools_pkg_url.as_str();
    service_fs.dir("svc").add_fidl_service(IncomingRequest::Launcher);

    service_fs.take_and_serve_directory_handle().context("failed to serve outgoing namespace")?;

    component::health().set_ok();
    debug!("Initialized.");

    let _inspect_server_task = inspect_runtime::publish(
        component::inspector(),
        inspect_runtime::PublishOptions::default(),
    );
    service_fs
        .for_each_concurrent(None, |IncomingRequest::Launcher(mut stream)| async move {
            while let Some(Ok(request)) = stream.next().await {
                match request {
                    fdash::LauncherRequest::ExploreComponentOverPty {
                        moniker,
                        pty,
                        mut tool_urls,
                        command,
                        ns_layout,
                        responder,
                    } => {
                        tool_urls.push(tools_pkg_url.to_string());
                        let result = crate::launch::component::explore_over_pty(
                            &moniker, pty, tool_urls, command, ns_layout,
                        )
                        .await
                        .map(|p| {
                            info!("launched Dash for instance {}", moniker);
                            notify_on_process_exit(p, responder.control_handle().clone());
                        });
                        let _ = responder.send(result);
                    }
                    fdash::LauncherRequest::ExploreComponentOverSocket {
                        moniker,
                        socket,
                        mut tool_urls,
                        command,
                        ns_layout,
                        responder,
                    } => {
                        tool_urls.push(tools_pkg_url.to_string());
                        let result = crate::launch::component::explore_over_socket(
                            &moniker, socket, tool_urls, command, ns_layout,
                        )
                        .await
                        .map(|p| {
                            info!("launched Dash for instance {}", moniker);
                            notify_on_process_exit(p, responder.control_handle().clone());
                        });
                        let _ = responder.send(result);
                    }
                    fdash::LauncherRequest::ExplorePackageOverSocket {
                        url,
                        subpackages,
                        socket,
                        mut tool_urls,
                        command,
                        responder,
                    } => {
                        tool_urls.push(tools_pkg_url.to_string());
                        let result = crate::launch::package::explore_over_socket(
                            fdash::FuchsiaPkgResolver::Full,
                            &url,
                            &subpackages,
                            socket,
                            tool_urls,
                            command,
                        )
                        .await
                        .map(|p| {
                            info!("launched Dash for package {} {}", url, subpackages.join(" "));
                            notify_on_process_exit(p, responder.control_handle().clone());
                        });
                        let _ = responder.send(result);
                    }
                    fdash::LauncherRequest::ExplorePackageOverSocket2 {
                        fuchsia_pkg_resolver,
                        url,
                        subpackages,
                        socket,
                        mut tool_urls,
                        command,
                        responder,
                    } => {
                        tool_urls.push(tools_pkg_url.to_string());
                        let result = crate::launch::package::explore_over_socket(
                            fuchsia_pkg_resolver,
                            &url,
                            &subpackages,
                            socket,
                            tool_urls,
                            command,
                        )
                        .await
                        .map(|p| {
                            info!("launched Dash for package {} {}", url, subpackages.join(" "));
                            notify_on_process_exit(p, responder.control_handle().clone());
                        });
                        let _ = responder.send(result);
                    }
                }
            }
        })
        .await;

    Ok(())
}

fn notify_on_process_exit(process: zx::Process, control_handle: fdash::LauncherControlHandle) {
    fasync::Task::spawn(async move {
        let _ = fasync::OnSignals::new(&process, zx::Signals::PROCESS_TERMINATED).await;
        match process.info() {
            Ok(info) => {
                let _ = control_handle
                    .send_on_terminated(info.return_code.try_into().unwrap())
                    .context("error sending OnTerminated event");
                info!("Dash process has terminated (exit code: {})", info.return_code);
            }
            Err(s) => {
                info!("Dash process has terminated (could not get exit code: {})", s);
                control_handle.shutdown();
            }
        }
    })
    .detach();
}
