// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::fmt;

use crate::utils::{self, on_off_to_bool};
use anyhow::{bail, Context as _, Error};
use argh::FromArgs;
use futures::prelude::*;
use {fidl_fuchsia_hardware_display as display, fidl_fuchsia_io as fio, fuchsia_fs};

async fn get_display_coordinator_path() -> anyhow::Result<String> {
    const DEVICE_CLASS_PATH: &'static str = "/dev/class/display-coordinator";
    let dir = fuchsia_fs::directory::open_in_namespace(DEVICE_CLASS_PATH, fio::Flags::empty())
        .context("open directory")?;
    let entries = fuchsia_fs::directory::readdir(&dir).await.context("read directory")?;
    let first_entry = entries.first().context("no valid display-coordinator")?;
    Ok(String::from(DEVICE_CLASS_PATH) + "/" + &first_entry.name)
}

/// Obtains a handle to the display entry point at the default hard-coded path.
async fn open_display_provider() -> Result<display::ProviderProxy, Error> {
    log::trace!("Opening display coordinator");

    let (proxy, server) = fidl::endpoints::create_proxy::<display::ProviderMarker>();
    let display_coordinator_path: String =
        get_display_coordinator_path().await.context("Failed to get display coordinator path")?;
    println!("Display coordinator path: {}", display_coordinator_path);

    fdio::service_connect(&display_coordinator_path, server.into_channel())
        .context("Failed to connect to default display coordinator provider")?;

    Ok(proxy)
}

/// The first stage in the process of connecting to the display driver system.
#[derive(Debug)]
struct DisplayProviderClient {
    provider: display::ProviderProxy,
}

/// The second stage in the process of connecting to the display driver system.
struct DisplayCoordinatorClient {
    coordinator: display::CoordinatorProxy,
    listener_requests: display::CoordinatorListenerRequestStream,
}

impl fmt::Debug for DisplayCoordinatorClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DisplayCoordinatorClient")
            .field("coordinator", &self.coordinator)
            .finish_non_exhaustive()
    }
}

/// The final stage in the process of connecting to the display driver system.
/// This stage supports all useful operations.
#[derive(Debug)]
struct DisplayClient {
    coordinator: display::CoordinatorProxy,

    display_infos: Vec<display::Info>,
}

impl DisplayProviderClient {
    pub async fn new() -> Result<DisplayProviderClient, Error> {
        let provider = open_display_provider().await?;
        Ok(DisplayProviderClient { provider })
    }

    // Opens the primary display coordinator from the provider at the default
    // hard-coded path.
    async fn open_display_coordinator(self) -> Result<DisplayCoordinatorClient, Error> {
        let (display_coordinator, coordinator_server) =
            fidl::endpoints::create_proxy::<display::CoordinatorMarker>();
        let (listener_client, listener_requests) =
            fidl::endpoints::create_request_stream::<display::CoordinatorListenerMarker>();

        let payload = display::ProviderOpenCoordinatorWithListenerForPrimaryRequest {
            coordinator: Some(coordinator_server),
            coordinator_listener: Some(listener_client),
            __source_breaking: fidl::marker::SourceBreaking,
        };

        let () = utils::flatten_zx_error(
            self.provider.open_coordinator_with_listener_for_primary(payload).await,
        )
        .context("Failed to get display Coordinator from Provider")?;

        Ok(DisplayCoordinatorClient { coordinator: display_coordinator, listener_requests })
    }
}

impl DisplayCoordinatorClient {
    /// Returns when the display coordinator sends the list of connected displays.
    async fn wait_for_display_infos(&mut self) -> Result<Vec<display::Info>, Error> {
        log::trace!("Waiting for events from the display coordinator");

        let listener_requests = &mut self.listener_requests;
        let mut stream = listener_requests.try_filter_map(|event| match event {
            display::CoordinatorListenerRequest::OnDisplaysChanged {
                added,
                removed: _,
                control_handle: _,
            } => future::ok(Some(added)),
            _ => future::ok(None),
        });
        let displays = stream.try_next().await?.context("failed to get display streams")?;
        return Ok(displays);
    }

    async fn into_display_client(mut self) -> Result<DisplayClient, Error> {
        let display_infos = self.wait_for_display_infos().await?;

        Ok(DisplayClient { coordinator: self.coordinator, display_infos: display_infos })
    }
}

impl DisplayClient {
    async fn set_panel_power(&mut self, power_state: bool) -> Result<(), Error> {
        if self.display_infos.is_empty() {
            bail!("fuchsia.hardware.display.Coordinator reported no connected displays");
        }

        let display_id = self.display_infos[0].id;
        log::trace!("First display's id: {}", display_id.value);

        log::trace!("Setting new power state");
        utils::flatten_zx_error(
            self.coordinator.set_display_power(&display_id.into(), power_state).await,
        )
        .context("Failed to set panel power state")
    }
}

/// Turn the panel on/off.
#[derive(FromArgs, Debug, PartialEq)]
#[argh(subcommand, name = "panel")]
pub struct PanelCmd {
    /// turn the panel's power on or off
    #[argh(option, long = "power", from_str_fn(on_off_to_bool))]
    set_power: Option<bool>,
}

impl PanelCmd {
    pub async fn exec(&self) -> Result<(), Error> {
        let display_provider_client = DisplayProviderClient::new().await?;
        let display_coordinator_client = display_provider_client.open_display_coordinator().await?;
        let mut display_client = display_coordinator_client.into_display_client().await?;

        if self.set_power.is_some() {
            display_client.set_panel_power(self.set_power.unwrap()).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;
    use fidl_fuchsia_hardware_display_types as display_types;
    use futures::StreamExt;

    #[fuchsia::test]
    async fn display_client_rpc_success() {
        let (provider, mut provider_request_stream) =
            fidl::endpoints::create_proxy_and_stream::<display::ProviderMarker>();
        let provider_client = DisplayProviderClient { provider };

        let test_future = async move {
            let display_coordinator = provider_client.open_display_coordinator().await.unwrap();
            let mut display_client = display_coordinator.into_display_client().await.unwrap();
            assert_matches!(display_client.set_panel_power(true).await, Ok(()));
        };

        let provider_service_future = async move {
            let (coordinator_server, coordinator_listener_client) =
                match provider_request_stream.next().await.unwrap() {
                    Ok(display::ProviderRequest::OpenCoordinatorWithListenerForPrimary {
                        payload:
                            display::ProviderOpenCoordinatorWithListenerForPrimaryRequest {
                                coordinator: Some(coordinator_server),
                                coordinator_listener: Some(coordinator_listener_client),
                                ..
                            },
                        responder,
                    }) => {
                        responder.send(Ok(())).unwrap();
                        (coordinator_server, coordinator_listener_client)
                    }
                    request => panic!("Unexpected request to Provider: {:?}", request),
                };

            let (mut coordinator_request_stream, _) =
                coordinator_server.into_stream_and_control_handle();

            let added_displays = &[display::Info {
                id: display_types::DisplayId { value: 42 },
                modes: vec![],
                pixel_format: vec![],
                manufacturer_name: "Test double".to_string(),
                monitor_name: "Display #1".to_string(),
                monitor_serial: "42".to_string(),
                horizontal_size_mm: 0,
                vertical_size_mm: 0,
                using_fallback_size: false,
            }];
            let coordinator_listener_proxy = coordinator_listener_client.into_proxy();
            coordinator_listener_proxy.on_displays_changed(added_displays, &[]).unwrap();

            match coordinator_request_stream.next().await.unwrap() {
                Ok(display::CoordinatorRequest::SetDisplayPower {
                    display_id: display_types::DisplayId { value: 42 },
                    responder,
                    ..
                }) => {
                    responder.send(Ok(())).unwrap();
                }
                request => panic!("Unexpected request to Coordinator: {:?}", request),
            }
        };
        futures::join!(test_future, provider_service_future);
    }

    #[fuchsia::test]
    async fn display_client_no_displays() {
        let (provider, mut provider_request_stream) =
            fidl::endpoints::create_proxy_and_stream::<display::ProviderMarker>();
        let provider_client = DisplayProviderClient { provider };

        let test_future = async move {
            let display_coordinator = provider_client.open_display_coordinator().await.unwrap();
            let mut display_client = display_coordinator.into_display_client().await.unwrap();

            let set_panel_power_result = display_client.set_panel_power(true).await;
            assert_matches!(set_panel_power_result, Err(_));
            assert_eq!(
                set_panel_power_result.unwrap_err().to_string(),
                "fuchsia.hardware.display.Coordinator reported no connected displays"
            );
        };

        let provider_service_future = async move {
            let (_coordinator_server, coordinator_listener_client) =
                match provider_request_stream.next().await.unwrap() {
                    Ok(display::ProviderRequest::OpenCoordinatorWithListenerForPrimary {
                        payload:
                            display::ProviderOpenCoordinatorWithListenerForPrimaryRequest {
                                coordinator: Some(coordinator_server),
                                coordinator_listener: Some(coordinator_listener_client),
                                ..
                            },
                        responder,
                    }) => {
                        responder.send(Ok(())).unwrap();
                        (coordinator_server, coordinator_listener_client)
                    }
                    request => panic!("Unexpected request to Provider: {:?}", request),
                };

            let coordinator_listener_proxy = coordinator_listener_client.into_proxy();
            coordinator_listener_proxy.on_displays_changed(&[], &[]).unwrap();
        };
        futures::join!(test_future, provider_service_future);
    }

    #[fuchsia::test]
    async fn display_client_error_opening_coordinator() {
        let (provider, mut provider_request_stream) =
            fidl::endpoints::create_proxy_and_stream::<display::ProviderMarker>();
        let provider_client = DisplayProviderClient { provider };

        let test_future = async move {
            let open_result = provider_client.open_display_coordinator().await;
            assert_matches!(open_result, Err(_));
            assert_eq!(
                open_result.unwrap_err().to_string(),
                "Failed to get display Coordinator from Provider"
            );
        };

        let provider_service_future = async move {
            match provider_request_stream.next().await.unwrap() {
                Ok(display::ProviderRequest::OpenCoordinatorWithListenerForPrimary {
                    responder,
                    ..
                }) => {
                    responder.send(Err(zx::sys::ZX_ERR_NOT_SUPPORTED)).unwrap();
                }
                request => panic!("Unexpected request to Provider: {:?}", request),
            };
        };
        futures::join!(test_future, provider_service_future);
    }

    #[fuchsia::test]
    async fn display_client_error_waiting_for_display_info() {
        let (provider, mut provider_request_stream) =
            fidl::endpoints::create_proxy_and_stream::<display::ProviderMarker>();
        let provider_client = DisplayProviderClient { provider };

        let test_future = async move {
            let display_coordinator = provider_client.open_display_coordinator().await.unwrap();
            let into_display_client_result = display_coordinator.into_display_client().await;
            assert_matches!(into_display_client_result, Err(_));
            assert_eq!(
                into_display_client_result.unwrap_err().to_string(),
                "failed to get display streams"
            );
        };

        let provider_service_future = async move {
            match provider_request_stream.next().await.unwrap() {
                Ok(display::ProviderRequest::OpenCoordinatorWithListenerForPrimary {
                    responder,
                    ..
                }) => {
                    responder.send(Ok(())).unwrap();
                }
                request => panic!("Unexpected request to Provider: {:?}", request),
            };
        };
        futures::join!(test_future, provider_service_future);
    }
}
