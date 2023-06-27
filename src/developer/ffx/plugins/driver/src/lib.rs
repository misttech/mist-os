// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use anyhow::{Context, Result};
use component_debug::capability;
use ffx_driver_args::DriverCommand;
use fho::{FfxMain, FfxTool, SimpleWriter};
use fidl::endpoints::{DiscoverableProtocolMarker, ProtocolMarker};
use fidl_fuchsia_developer_remotecontrol as rc;
use fidl_fuchsia_device_manager as fdm;
use fidl_fuchsia_driver_development as fdd;
use fidl_fuchsia_driver_playground as fdp;
use fidl_fuchsia_driver_registrar as fdr;
use fidl_fuchsia_io as fio;
use fidl_fuchsia_sys2 as fsys;
use fidl_fuchsia_test_manager as ftm;
use fuchsia_zircon_status::Status;

struct DriverConnector {
    remote_control: Option<rc::RemoteControlProxy>,
}

struct CapabilityOptions {
    capability_name: &'static str,
    default_capability_name_for_query: &'static str,
}

struct DiscoverableCapabilityOptions<P> {
    _phantom: std::marker::PhantomData<P>,
}

// #[derive(Default)] imposes a spurious P: Default bound.
impl<P> Default for DiscoverableCapabilityOptions<P> {
    fn default() -> Self {
        Self { _phantom: Default::default() }
    }
}

impl<P: DiscoverableProtocolMarker> Into<CapabilityOptions> for DiscoverableCapabilityOptions<P> {
    fn into(self) -> CapabilityOptions {
        CapabilityOptions {
            capability_name: P::PROTOCOL_NAME,
            default_capability_name_for_query: P::PROTOCOL_NAME,
        }
    }
}

impl DriverConnector {
    fn new(remote_control: Option<rc::RemoteControlProxy>) -> Self {
        Self { remote_control }
    }

    async fn get_component_with_capability<S: ProtocolMarker>(
        &self,
        moniker: &str,
        capability_options: impl Into<CapabilityOptions>,
        select: bool,
    ) -> Result<S::Proxy> {
        async fn remotecontrol_connect<S: ProtocolMarker>(
            remote_control: &rc::RemoteControlProxy,
            moniker: &str,
            capability: &str,
        ) -> Result<S::Proxy> {
            let (proxy, server_end) = fidl::endpoints::create_proxy::<S>()
                .with_context(|| format!("failed to create proxy to {}", S::DEBUG_NAME))?;
            remote_control
                .connect_capability(
                    moniker,
                    capability,
                    server_end.into_channel(),
                    fio::OpenFlags::empty(),
                )
                .await?
                .map_err(|e| {
                    anyhow::anyhow!(
                        "failed to connect to {} at {} as {}: {:?}",
                        S::DEBUG_NAME,
                        moniker,
                        capability,
                        e
                    )
                })?;
            Ok(proxy)
        }

        // Gets monikers for components that expose a capability matching the given |query|.
        // This moniker is eventually converted into a selector and is used to connecting to
        // the capability.
        async fn find_components_with_capability(
            rcs_proxy: &rc::RemoteControlProxy,
            query: &str,
        ) -> Result<Vec<String>> {
            let (query_proxy, query_server) =
                fidl::endpoints::create_proxy::<fsys::RealmQueryMarker>()
                    .context("creating realm query proxy")?;
            rcs_proxy
                .root_realm_query(query_server)
                .await?
                .map_err(|i| Status::ok(i).unwrap_err())
                .context("opening query")?;

            Ok(capability::get_all_route_segments(query.to_string(), &query_proxy)
                .await?
                .iter()
                .filter_map(|segment| {
                    if let capability::RouteSegment::ExposeBy { moniker, .. } = segment {
                        Some(moniker.to_string())
                    } else {
                        None
                    }
                })
                .collect())
        }

        /// Find the components that expose a given capability, and let the user
        /// request which component they would like to connect to.
        async fn user_choose_selector(
            remote_control: &rc::RemoteControlProxy,
            capability: &str,
        ) -> Result<String> {
            let capabilities = find_components_with_capability(&remote_control, capability).await?;
            println!("Please choose which component to connect to:");
            for (i, component) in capabilities.iter().enumerate() {
                println!("    {}: {}", i, component)
            }

            let mut line_editor = rustyline::Editor::<()>::new();
            loop {
                let line = line_editor.readline("$ ")?;
                let choice = line.trim().parse::<usize>();
                if choice.is_err() {
                    println!("Error: please choose a value.");
                    continue;
                }
                let choice = choice.unwrap();
                if choice >= capabilities.len() {
                    println!("Error: please choose a correct value.");
                    continue;
                }
                // We have to escape colons in the capability name to distinguish them from the
                // syntactically meaningful colons in the ':expose:" string.
                return Ok(capabilities[choice].clone());
            }
        }

        let CapabilityOptions { capability_name, default_capability_name_for_query } =
            capability_options.into();

        if let Some(ref remote_control) = self.remote_control {
            let (moniker, capability): (String, &str) = match select {
                true => {
                    (user_choose_selector(remote_control, capability_name).await?, capability_name)
                }
                false => (moniker.to_string(), default_capability_name_for_query),
            };
            remotecontrol_connect::<S>(&remote_control, &moniker, &capability).await
        } else {
            anyhow::bail!("Failed to get remote control proxy");
        }
    }
}

#[async_trait::async_trait]
impl driver_connector::DriverConnector for DriverConnector {
    async fn get_driver_development_proxy(
        &self,
        select: bool,
    ) -> Result<fdd::DriverDevelopmentProxy> {
        self.get_component_with_capability::<fdd::DriverDevelopmentMarker>(
            "/bootstrap/driver_manager",
            DiscoverableCapabilityOptions::<fdd::DriverDevelopmentMarker>::default(),
            select,
        )
        .await
        .context("Failed to get driver development component")
    }

    async fn get_dev_proxy(&self, select: bool) -> Result<fio::DirectoryProxy> {
        self.get_component_with_capability::<fio::DirectoryMarker>(
            "/bootstrap/devfs",
            CapabilityOptions {
                capability_name: "dev",
                default_capability_name_for_query: "dev-topological",
            },
            select,
        )
        .await
        .context("Failed to get dev component")
    }

    async fn get_device_watcher_proxy(&self) -> Result<fdm::DeviceWatcherProxy> {
        self.get_component_with_capability::<fdm::DeviceWatcherMarker>(
            "/bootstrap/driver_manager",
            CapabilityOptions {
                capability_name: "fuchsia.hardware.usb.DeviceWatcher",
                default_capability_name_for_query: "fuchsia.hardware.usb.DeviceWatcher",
            },
            false,
        )
        .await
        .context("Failed to get device watcher component")
    }

    async fn get_driver_registrar_proxy(&self, select: bool) -> Result<fdr::DriverRegistrarProxy> {
        self.get_component_with_capability::<fdr::DriverRegistrarMarker>(
            "/bootstrap/driver_index",
            DiscoverableCapabilityOptions::<fdr::DriverRegistrarMarker>::default(),
            select,
        )
        .await
        .context("Failed to get driver registrar component")
    }

    async fn get_tool_runner_proxy(&self, select: bool) -> Result<fdp::ToolRunnerProxy> {
        self.get_component_with_capability::<fdp::ToolRunnerMarker>(
            "/core/driver_playground",
            DiscoverableCapabilityOptions::<fdp::ToolRunnerMarker>::default(),
            select,
        )
        .await
        .context("Failed to get tool runner component")
    }

    async fn get_run_builder_proxy(&self) -> Result<ftm::RunBuilderProxy> {
        self.get_component_with_capability::<ftm::RunBuilderMarker>(
            "/core/test_manager",
            DiscoverableCapabilityOptions::<ftm::RunBuilderMarker>::default(),
            false,
        )
        .await
        .context("Failed to get RunBuilder component")
    }
}

#[derive(FfxTool)]
pub struct DriverTool {
    remote_control: fho::Result<rc::RemoteControlProxy>,
    #[command]
    cmd: DriverCommand,
}

fho::embedded_plugin!(DriverTool);

#[async_trait::async_trait(?Send)]
impl FfxMain for DriverTool {
    type Writer = SimpleWriter;

    async fn main(self, mut writer: Self::Writer) -> fho::Result<()> {
        driver_tools::driver(
            self.cmd.into(),
            DriverConnector::new(self.remote_control.ok()),
            &mut writer,
        )
        .await
        .map_err(Into::into)
    }
}
