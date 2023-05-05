// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{bail, Context as _, Error};
use argh::FromArgs;
use fdio;
use fidl;
use fidl_fuchsia_hardware_display as display;
use futures::prelude::*;
use tracing;

use crate::utils::{self, on_off_to_bool};

/// Obtains a handle to the display entry point at the default hard-coded path.
fn open_display_provider() -> Result<display::ProviderProxy, Error> {
    tracing::trace!("Opening display coordinator");

    let (proxy, server) = fidl::endpoints::create_proxy::<display::ProviderMarker>()
        .context("Failed to create fuchsia.hardware.display.Provider proxy")?;
    fdio::service_connect("/dev/class/display-controller/000", server.into_channel())
        .context("Failed to connect to default display coordinator provider")?;

    Ok(proxy)
}

/// The first stage in the process of connecting to the display driver system.
#[derive(Debug)]
struct DisplayProviderClient {
    provider: display::ProviderProxy,
}

/// The second stage in the process of connecting to the display driver system.
///
#[derive(Debug)]
struct DisplayCoordinatorClient {
    coordinator: display::CoordinatorProxy,
}

/// The final stage in the process of connecting to the display driver system.
/// This stage supports all useful operations.
#[derive(Debug)]
struct DisplayClient {
    coordinator: display::CoordinatorProxy,

    display_infos: Vec<display::Info>,
}

impl DisplayProviderClient {
    pub fn new() -> Result<DisplayProviderClient, Error> {
        let provider = open_display_provider()?;
        Ok(DisplayProviderClient { provider })
    }

    // Opens the primary display coordinator from the provider at the default
    // hard-coded path.
    async fn open_display_coordinator(self) -> Result<DisplayCoordinatorClient, Error> {
        let (display_coordinator, coordinator_server) =
            fidl::endpoints::create_proxy::<display::CoordinatorMarker>()
                .context("Failed to create fuchsia.hardware.display.Coordinator proxy")?;

        utils::flatten_zx_status(
            self.provider.open_coordinator_for_primary(coordinator_server).await,
        )
        .context("Failed to get display Coordinator from Provider")?;

        Ok(DisplayCoordinatorClient { coordinator: display_coordinator })
    }
}

impl DisplayCoordinatorClient {
    /// Returns when the display coordinator sends the list of connected displays.
    async fn wait_for_display_infos(&mut self) -> Result<Vec<display::Info>, Error> {
        tracing::trace!("Waiting for events from the display coordinator");

        let event_stream: display::CoordinatorEventStream = self.coordinator.take_event_stream();

        let display_infos = event_stream
            .try_filter_map(|event| {
                futures::future::ok(match event {
                    display::CoordinatorEvent::OnDisplaysChanged {
                        added: display_infos, ..
                    } => Some(display_infos),
                    _ => None,
                })
            })
            .next()
            .await
            .context("Failed to get events from fuchsia.hardware.display.Coordinator")??;

        return Ok(display_infos);
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
        tracing::trace!("First display's id: {}", display_id);

        tracing::trace!("Setting new power state");
        utils::flatten_zx_error(self.coordinator.set_display_power(display_id, power_state).await)
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
        let display_provider_client = DisplayProviderClient::new()?;
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
    use fuchsia_zircon as zx;
    use futures::StreamExt;

    #[fuchsia::test]
    async fn display_client_rpc_success() {
        let (provider, mut provider_request_stream) =
            fidl::endpoints::create_proxy_and_stream::<display::ProviderMarker>().unwrap();
        let provider_client = DisplayProviderClient { provider };

        let test_future = async move {
            let display_coordinator = provider_client.open_display_coordinator().await.unwrap();
            let mut display_client = display_coordinator.into_display_client().await.unwrap();
            assert_matches!(display_client.set_panel_power(true).await, Ok(()));
        };

        let provider_service_future = async move {
            let coordinator_server = match provider_request_stream.next().await.unwrap() {
                Ok(display::ProviderRequest::OpenCoordinatorForPrimary {
                    coordinator: coordinator_server,
                    responder,
                }) => {
                    responder.send(zx::sys::ZX_OK).unwrap();
                    coordinator_server
                }
                request => panic!("Unexpected request to Provider: {:?}", request),
            };

            let (mut coordinator_request_stream, coordinator_control) =
                coordinator_server.into_stream_and_control_handle().unwrap();

            let added_displays = &[display::Info {
                id: 42,
                modes: vec![],
                pixel_format: vec![],
                cursor_configs: vec![],
                manufacturer_name: "Test double".to_string(),
                monitor_name: "Display #1".to_string(),
                monitor_serial: "42".to_string(),
                horizontal_size_mm: 0,
                vertical_size_mm: 0,
                using_fallback_size: false,
            }];
            coordinator_control.send_on_displays_changed(added_displays, &mut []).unwrap();

            match coordinator_request_stream.next().await.unwrap() {
                Ok(display::CoordinatorRequest::SetDisplayPower {
                    display_id: 42,
                    responder,
                    ..
                }) => {
                    responder.send(&mut Ok(())).unwrap();
                }
                request => panic!("Unexpected request to Coordinator: {:?}", request),
            }
        };
        futures::join!(test_future, provider_service_future);
    }

    #[fuchsia::test]
    async fn display_client_no_displays() {
        let (provider, mut provider_request_stream) =
            fidl::endpoints::create_proxy_and_stream::<display::ProviderMarker>().unwrap();
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
            let coordinator_server = match provider_request_stream.next().await.unwrap() {
                Ok(display::ProviderRequest::OpenCoordinatorForPrimary {
                    coordinator: coordinator_server,
                    responder,
                }) => {
                    responder.send(zx::sys::ZX_OK).unwrap();
                    coordinator_server
                }
                request => panic!("Unexpected request to Provider: {:?}", request),
            };

            let (_, coordinator_control) =
                coordinator_server.into_stream_and_control_handle().unwrap();
            coordinator_control.send_on_displays_changed(&[], &mut []).unwrap();
        };
        futures::join!(test_future, provider_service_future);
    }

    #[fuchsia::test]
    async fn display_client_error_opening_coordinator() {
        let (provider, mut provider_request_stream) =
            fidl::endpoints::create_proxy_and_stream::<display::ProviderMarker>().unwrap();
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
                Ok(display::ProviderRequest::OpenCoordinatorForPrimary { responder, .. }) => {
                    responder.send(zx::sys::ZX_ERR_NOT_SUPPORTED).unwrap();
                }
                request => panic!("Unexpected request to Provider: {:?}", request),
            };
        };
        futures::join!(test_future, provider_service_future);
    }

    #[fuchsia::test]
    async fn display_client_error_waiting_for_display_info() {
        let (provider, mut provider_request_stream) =
            fidl::endpoints::create_proxy_and_stream::<display::ProviderMarker>().unwrap();
        let provider_client = DisplayProviderClient { provider };

        let test_future = async move {
            let display_coordinator = provider_client.open_display_coordinator().await.unwrap();
            let into_display_client_result = display_coordinator.into_display_client().await;
            assert_matches!(into_display_client_result, Err(_));
            assert_eq!(
                into_display_client_result.unwrap_err().to_string(),
                "Failed to get events from fuchsia.hardware.display.Coordinator"
            );
        };

        let provider_service_future = async move {
            match provider_request_stream.next().await.unwrap() {
                Ok(display::ProviderRequest::OpenCoordinatorForPrimary { responder, .. }) => {
                    responder.send(zx::sys::ZX_OK).unwrap();
                }
                request => panic!("Unexpected request to Provider: {:?}", request),
            };
        };
        futures::join!(test_future, provider_service_future);
    }
}
