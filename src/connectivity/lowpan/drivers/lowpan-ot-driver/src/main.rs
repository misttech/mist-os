// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! LoWPAN OpenThread Driver
#![warn(rust_2018_idioms)]

use anyhow::Error;
use fidl_fuchsia_factory_lowpan::{FactoryRegisterMarker, FactoryRegisterProxyInterface};
use fidl_fuchsia_lowpan_driver::{RegisterMarker, RegisterProxyInterface};
use fidl_fuchsia_lowpan_spinel::{
    DeviceMarker as SpinelDeviceMarker, DeviceProxy as SpinelDeviceProxy,
    DeviceSetupMarker as SpinelDeviceSetupMarker,
};
use fuchsia_component::client::{connect_to_protocol, connect_to_protocol_at};

use lowpan_driver_common::net::*;
use lowpan_driver_common::spinel::SpinelDeviceSink;
use lowpan_driver_common::{register_and_serve_driver, register_and_serve_driver_factory};
use openthread_fuchsia::Platform as OtPlatform;

use config::Config;
use fidl::endpoints::create_proxy;

use crate::driver::OtDriver;
use crate::prelude::*;
use fuchsia as _;
use std::ffi::CString;
use std::num::NonZeroU32;

mod bootstrap;
mod config;
mod convert_ext;
mod driver;

#[macro_use]
mod prelude {
    #![allow(unused_imports)]

    pub use crate::convert_ext::FromExt as _;
    pub use crate::convert_ext::IntoExt as _;
    pub use crate::Result;
    pub use anyhow::{bail, format_err, Context as _};
    pub use fasync::TimeoutExt as _;
    pub use fidl_fuchsia_net_ext as fnet_ext;
    pub use fuchsia_async as fasync;
    pub use fuchsia_zircon as fz;
    pub use fuchsia_zircon_status::Status as ZxStatus;
    pub use futures::future::BoxFuture;
    pub use futures::stream::BoxStream;
    pub use lowpan_driver_common::pii::MarkPii;
    pub use lowpan_driver_common::ZxResult;
    pub use net_declare::{fidl_ip, fidl_ip_v6};
    pub use std::convert::TryInto;
    pub use std::fmt::Debug;
    pub use tracing::{debug, error, info, trace, warn};

    pub use futures::prelude::*;
    pub use openthread::prelude::*;
}

pub type Result<T = (), E = anyhow::Error> = std::result::Result<T, E>;

const MAX_EXPONENTIAL_BACKOFF_DELAY_SEC: i64 = 180;
const RESET_EXPONENTIAL_BACKOFF_TIMER_MIN: i64 = 5;

impl Config {
    async fn open_spinel_device_proxy(&self) -> Result<SpinelDeviceProxy, Error> {
        use std::path::Path;

        let path = self.ot_radio_path.as_deref().unwrap_or("/dev/class/ot-radio");

        // If we are just given a directory, try to infer the full path.
        let spinel_device_setup_proxy = if Path::new(path).is_dir() {
            let directory_proxy =
                fuchsia_fs::directory::open_in_namespace(path, fuchsia_fs::OpenFlags::empty())?;

            let entries = fuchsia_fs::directory::readdir(&directory_proxy).await?;

            // Should have 1 device that implements OT_RADIO
            match entries.as_slice() {
                [entry] => {
                    info!("Attempting to use Spinel RCP at {}/{}", path, entry.name.as_str());

                    fuchsia_component::client::connect_to_named_protocol_at_dir_root::<
                        SpinelDeviceSetupMarker,
                    >(&directory_proxy, entry.name.as_str())
                }
                ot_radio_devices => {
                    return Err(format_err!(
                        "There are {} devices in {}, expecting only one",
                        ot_radio_devices.len(),
                        path
                    ));
                }
            }
        } else {
            info!("Attempting to use Spinel RCP at {}", path);

            fuchsia_component::client::connect_to_protocol_at_path::<SpinelDeviceSetupMarker>(path)
        }
        .context("Error opening Spinel RCP")?;

        let (client_side, server_side) = fidl::endpoints::create_endpoints::<SpinelDeviceMarker>();

        spinel_device_setup_proxy
            .set_channel(server_side)
            .await?
            .map_err(ZxStatus::from_raw)
            .context(
            "Unable to set server-side FIDL channel via spinel_device_setup_proxy.set_channel()",
        )?;

        Ok(client_side.into_proxy()?)
    }

    fn get_backbone_netif_index_by_config(&self) -> Option<ot::NetifIndex> {
        if self.backbone_name.as_ref().unwrap().is_empty() {
            info!("Backbone interface is disabled");
            return None;
        }

        let c_name = CString::new(self.backbone_name.as_ref().unwrap().as_bytes().to_vec())
            .expect("Invalid backbone interface name");

        // SAFETY: Calling `if_name_toindex` is safe assuming that the C-string pointer
        //         being passed into it is valid, which is guaranteed by `CString::as_ptr()`.
        let index = unsafe { libc::if_nametoindex(c_name.as_ptr()) };

        if index == 0 {
            error!("Unable to look up index of interface {:?}", self.backbone_name);
            return None;
        }

        info!("Backbone interface is {:?} (index {})", self.backbone_name, index);

        Some(index)
    }

    fn get_backbone_netif_index_by_wlan_availability(&self) -> Option<ot::NetifIndex> {
        let state = connect_to_protocol::<fidl_fuchsia_net_interfaces::StateMarker>()
            .expect("error connecting to StateMarker");
        let (watcher_client, watcher_server) =
            create_proxy::<fidl_fuchsia_net_interfaces::WatcherMarker>()
                .expect("error connecting to WatcherMarker");
        state
            .get_watcher(&fidl_fuchsia_net_interfaces::WatcherOptions::default(), watcher_server)
            .expect("error getting interface watcher");

        let get_nicid_fut = async move {
            loop {
                match watcher_client.watch().await.expect("") {
                    fidl_fuchsia_net_interfaces::Event::Existing(
                        fidl_fuchsia_net_interfaces::Properties {
                            id,
                            name,
                            online,
                            device_class,
                            ..
                        },
                    ) => {
                        info!(
                            "NICID: {:?}, name: {:?}, online: {:?}, device_class: {:?}",
                            id, name, online, device_class
                        );
                        if let (
                            Some(fidl_fuchsia_net_interfaces::DeviceClass::Device(
                                fidl_fuchsia_hardware_network::DeviceClass::Wlan,
                            )),
                            Some(true),
                        ) = (device_class, online)
                        {
                            return Some(id.unwrap_or(0) as ot::NetifIndex);
                        }
                    }
                    fidl_fuchsia_net_interfaces::Event::Idle(
                        fidl_fuchsia_net_interfaces::Empty {},
                    ) => {
                        break;
                    }
                    _ => {}
                }
            }
            None
        };

        futures::executor::block_on(get_nicid_fut)
    }

    fn get_backbone_netif_index(&self) -> Option<ot::NetifIndex> {
        if self.backbone_name.is_none() {
            self.get_backbone_netif_index_by_wlan_availability()
        } else {
            self.get_backbone_netif_index_by_config()
        }
    }

    /// Async method which returns the future that runs the driver.
    async fn prepare_to_run(&self) -> Result<impl Future<Output = Result<(), Error>>, Error> {
        let spinel_device_proxy = self.open_spinel_device_proxy().await?;
        debug!("Spinel device proxy initialized");

        let spinel_sink = SpinelDeviceSink::new(spinel_device_proxy);
        let spinel_stream = spinel_sink.take_stream();

        let netif = TunNetworkInterface::try_new(Some(self.name.clone()))
            .await
            .context("Unable to start TUN driver")?;

        let mut builder = OtPlatform::build().thread_netif_index(
            netif
                .get_index()
                .try_into()
                .expect("Network interface index is too large for OpenThread"),
        );

        netif
            .set_ipv6_forwarding_enabled(true)
            .await
            .expect("Unable to enable ipv6 packet forwarding on lowpan interface");

        netif
            .set_ipv4_forwarding_enabled(true)
            .await
            .expect("Unable to enable ipv4 packet forwarding on lowpan interface");

        let backbone_netif_index = self.get_backbone_netif_index();
        let backbone_if = BackboneNetworkInterface::new(backbone_netif_index.unwrap_or(0).into());

        if let Some(index) = backbone_netif_index {
            builder = builder.backbone_netif_index(index);
        }

        let ot_instance = ot::Instance::new(builder.init(spinel_sink, spinel_stream));

        // TODO: might switch to check if the infra_if instance is constructed successfully
        if let Some(index) = backbone_netif_index.and_then(NonZeroU32::new) {
            ot_instance
                .border_routing_init(index.get(), true)
                .context("Unable to initialize OpenThread border routing")?;

            ot_instance.border_routing_set_enabled(true).context("border_routing_set_enabled")?;

            ot_instance.set_backbone_router_enabled(true);
        } else {
            warn!("Backbone interface not set, border routing not supported");
        }

        let driver_future = run_driver(
            self.name.clone(),
            connect_to_protocol_at::<RegisterMarker>(self.service_prefix.as_str())
                .context("Failed to connect to Lowpan Registry service")?,
            connect_to_protocol_at::<FactoryRegisterMarker>(self.service_prefix.as_str()).ok(),
            ot_instance,
            netif,
            backbone_if,
        );

        Ok(driver_future)
    }
}

async fn run_driver<N, RP, RFP, NI, BI>(
    name: N,
    registry: RP,
    factory_registry: Option<RFP>,
    ot_instance: OtInstanceBox,
    net_if: NI,
    backbone_if: BI,
) -> Result<(), Error>
where
    N: AsRef<str>,
    RP: RegisterProxyInterface,
    RFP: FactoryRegisterProxyInterface,
    NI: NetworkInterface + Debug,
    BI: BackboneInterface,
{
    let name = name.as_ref();
    let mut driver = OtDriver::new(ot_instance, net_if, backbone_if);

    driver.start_multicast_routing_manager();

    driver.init_nat64();

    let driver_ref = &driver;

    let lowpan_device_task = register_and_serve_driver(name, registry, driver_ref);

    info!("Registered OpenThread LoWPAN device {}", name);

    let lowpan_device_factory_task = async move {
        if let Some(factory_registry) = factory_registry {
            if let Err(err) =
                register_and_serve_driver_factory(name, factory_registry, driver_ref).await
            {
                warn!("Unable to register and serve factory commands for {}: {:?}", name, err);
            }
        }

        // If the factory interface throws an error, don't kill the driver;
        // just let the rest keep running.
        futures::future::pending::<Result<(), Error>>().await
    };

    // All three of these tasks will run indefinitely
    // as long as there are no irrecoverable problems.
    //
    // We use `stream::select_all` here so that only the
    // futures that actually need to be polled get polled.
    futures::stream::select_all([
        driver.main_loop_stream().boxed(),
        lowpan_device_task.into_stream().boxed(),
        lowpan_device_factory_task.into_stream().boxed(),
    ])
    .try_collect::<()>()
    .await?;

    info!("OpenThread LoWPAN device {} has shutdown.", name);

    Ok(())
}

// The OpenThread platform implementation currently requires a multithreaded executor.
#[fasync::run(10)]
async fn main() -> Result<(), Error> {
    use std::path::Path;

    let config = Config::try_new().context("Config::try_new")?;

    // Use the diagnostics_log library directly rather than e.g. the #[fuchsia::main] macro on
    // the main function, so that we can specify the logging severity level at runtime based on a
    // command line argument.
    diagnostics_log::initialize(
        diagnostics_log::PublishOptions::default().minimum_severity(config.log_level),
    )?;

    // Make sure OpenThread is logging at a similar level as the rest of the system.
    ot::set_logging_level(openthread_fuchsia::logging::ot_log_level_from(config.log_level));

    if Path::new("/config/data/bootstrap_config.json").exists() {
        warn!("Bootstrapping thread. Skipping ot-driver loop.");
        return bootstrap::bootstrap_thread().await;
    }

    let mut attempt_count = 0;
    loop {
        info!("Starting LoWPAN OT Driver");

        let driver_future = config
            .prepare_to_run()
            .inspect_err(|e| error!("main:prepare_to_run: {:?}", e))
            .await
            .context("main:prepare_to_run")?
            .boxed();

        let start_timestamp = fasync::Time::now();

        let ret = driver_future.await.context("main:driver_task");

        if (fasync::Time::now() - start_timestamp).into_minutes()
            >= RESET_EXPONENTIAL_BACKOFF_TIMER_MIN
        {
            // If the past run has been running for `RESET_EXPONENTIAL_BACKOFF_TIMER_MIN`
            // minutes or longer, then we go ahead and reset the attempt count.
            attempt_count = 0;
        }

        if config.max_auto_restarts <= attempt_count {
            panic!("Failed {} attempts to restart OpenThread: {ret:?}", config.max_auto_restarts);
        }

        // Implement an exponential backoff for restarts.
        let delay = (1 << attempt_count).min(MAX_EXPONENTIAL_BACKOFF_DELAY_SEC);

        if ret
            .as_ref()
            .map_err(|err| err.is::<driver::ResetRequested>() || err.is::<BackboneNetworkChanged>())
            .err()
            .unwrap_or(false)
        {
            // This is an expected OpenThread reset.
            warn!("OpenThread Reset: {:?}", ret);
        } else {
            error!("Unexpected shutdown: {:?}", ret);
            warn!("Will attempt to restart in {} seconds.", delay);

            fasync::Timer::new(fasync::Time::after(fz::Duration::from_seconds(delay))).await;

            attempt_count += 1;

            info!("Restart attempt {} ({} max)", attempt_count, config.max_auto_restarts);
        }
    }
}
