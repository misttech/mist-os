// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::Context as _,
    fidl_fuchsia_io as fio, fidl_fuchsia_wlan_device as fidl_wlan_dev,
    fuchsia_fs::directory::{WatchEvent, WatchMessage, Watcher},
    futures::{
        future::TryFutureExt as _,
        stream::{Stream, TryStreamExt as _},
    },
    std::hash::{Hash as _, Hasher as _},
    tracing::error,
};

pub struct NewPhyDevice {
    pub id: u16,
    pub proxy: fidl_wlan_dev::PhyProxy,
    pub device_path: String,
}

pub fn watch_phy_devices<'a>(
    device_directory: &'a str,
) -> Result<impl Stream<Item = Result<NewPhyDevice, anyhow::Error>> + 'a, anyhow::Error> {
    let directory =
        fuchsia_fs::directory::open_in_namespace(device_directory, fio::OpenFlags::empty())
            .context("open directory")?;
    Ok(async move {
        let watcher = Watcher::new(&directory).await.context("create watcher")?;
        Ok(watcher.err_into().try_filter_map(move |WatchMessage { event, filename }| {
            futures::future::ready((|| {
                match event {
                    WatchEvent::ADD_FILE | WatchEvent::EXISTING => {}
                    _ => return Ok(None),
                };
                let filename = match filename.as_path().to_str() {
                    Some(filename) => filename,
                    None => return Ok(None),
                };
                if filename == "." {
                    return Ok(None);
                }
                let (proxy, server_end) =
                    fidl::endpoints::create_proxy().context("create proxy")?;
                let connector = fuchsia_component::client::connect_to_named_protocol_at_dir_root::<
                    fidl_fuchsia_wlan_device::ConnectorMarker,
                >(&directory, filename)
                .context("connect to device")?;
                let () = match connector.connect(server_end) {
                    Ok(()) => (),
                    Err(e) => {
                        return match e {
                            fidl::Error::ClientChannelClosed { .. } => {
                                error!("Error opening '{}': {}", filename, e);
                                Ok(None)
                            }
                            e => Err(e.into()),
                        }
                    }
                };
                // TODO(https://fxbug.dev/124740): remove the assumption that devices have numeric IDs.
                let mut s = std::collections::hash_map::DefaultHasher::new();
                let () = filename.hash(&mut s);
                let mut s: u64 = s.finish();
                let mut id: u16 = 0;
                while s != 0 {
                    id |= s as u16;
                    s = s >> 16;
                }
                Ok(Some(NewPhyDevice {
                    id,
                    proxy,
                    device_path: format!("{}/{}", device_directory, filename),
                }))
            })())
        }))
    }
    .try_flatten_stream())
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::PHY_PATH,
        fidl_fuchsia_driver_test as fdt, fidl_fuchsia_wlan_common as fidl_wlan_common,
        fidl_fuchsia_wlan_device::{self as fidl_wlan_dev},
        fidl_fuchsia_wlan_ieee80211 as fidl_ieee80211, fidl_fuchsia_wlan_tap as fidl_wlantap,
        fuchsia_async as fasync,
        fuchsia_component::client::connect_to_protocol,
        fuchsia_zircon::DurationNum as _,
        futures::{pin_mut, poll, stream::StreamExt as _, task::Poll},
        std::convert::TryInto as _,
        wlan_common::{ie::*, test_utils::ExpectWithin},
        wlantap_client,
        zerocopy::AsBytes,
    };

    #[fasync::run_singlethreaded(test)]
    async fn watch_phys() {
        // Connect to and start driver test realm
        let driver_test_realm_proxy = connect_to_protocol::<fdt::RealmMarker>()
            .expect("Failed to connect to driver test realm");

        let _ = driver_test_realm_proxy
            .start(fdt::RealmArgs { use_driver_framework_v2: Some(true), ..Default::default() })
            .await
            .expect("FIDL error when starting driver test realm");

        let phy_watcher = watch_phy_devices(PHY_PATH).expect("Failed to create phy_watcher");
        pin_mut!(phy_watcher);

        let wlantap =
            wlantap_client::Wlantap::open().await.expect("Failed to connect to wlantapctl");

        // Create an intentionally unused variable instead of a plain
        // underscore. Otherwise, this end of the channel will be
        // dropped and cause the phy device to begin unbinding.
        let wlantap_phy =
            wlantap.create_phy(create_wlantap_config()).await.expect("failed to create PHY");
        phy_watcher
            .next()
            .expect_within(5.seconds(), "phy_watcher did not respond")
            .await
            .expect("phy_watcher ended without yielding a phy")
            .expect("phy_watcher returned an error");
        if let Poll::Ready(..) = poll!(phy_watcher.next()) {
            panic!("phy_watcher found more than one phy");
        }

        let () = wlantap_phy.shutdown().await.expect("shutdown operation failed");
    }

    fn create_wlantap_config() -> fidl_wlantap::WlantapPhyConfig {
        fidl_wlantap::WlantapPhyConfig {
            sta_addr: [1; 6],
            supported_phys: vec![
                fidl_wlan_common::WlanPhyType::Dsss,
                fidl_wlan_common::WlanPhyType::Hr,
                fidl_wlan_common::WlanPhyType::Ofdm,
                fidl_wlan_common::WlanPhyType::Erp,
                fidl_wlan_common::WlanPhyType::Ht,
            ],
            mac_role: fidl_wlan_common::WlanMacRole::Client,
            hardware_capability: 0,
            bands: vec![create_2_4_ghz_band_info()],
            name: String::from("devwatchtap"),
            quiet: false,
            discovery_support: fidl_wlan_common::DiscoverySupport {
                scan_offload: fidl_wlan_common::ScanOffloadExtension {
                    supported: false,
                    scan_cancel_supported: false,
                },
                probe_response_offload: fidl_wlan_common::ProbeResponseOffloadExtension {
                    supported: false,
                },
            },
            mac_sublayer_support: fidl_wlan_common::MacSublayerSupport {
                rate_selection_offload: fidl_wlan_common::RateSelectionOffloadExtension {
                    supported: false,
                },
                data_plane: fidl_wlan_common::DataPlaneExtension {
                    data_plane_type: fidl_wlan_common::DataPlaneType::EthernetDevice,
                },
                device: fidl_wlan_common::DeviceExtension {
                    is_synthetic: false,
                    mac_implementation_type: fidl_wlan_common::MacImplementationType::Softmac,
                    tx_status_report_supported: false,
                },
            },
            security_support: fidl_wlan_common::SecuritySupport {
                sae: fidl_wlan_common::SaeFeature {
                    driver_handler_supported: false,
                    sme_handler_supported: false,
                },
                mfp: fidl_wlan_common::MfpFeature { supported: false },
            },
            spectrum_management_support: fidl_wlan_common::SpectrumManagementSupport {
                dfs: fidl_wlan_common::DfsFeature { supported: false },
            },
        }
    }

    fn create_2_4_ghz_band_info() -> fidl_wlan_dev::BandInfo {
        fidl_wlan_dev::BandInfo {
            band: fidl_wlan_common::WlanBand::TwoGhz,
            ht_caps: Some(Box::new(fidl_ieee80211::HtCapabilities {
                bytes: fake_ht_capabilities().as_bytes().try_into().unwrap(),
            })),
            vht_caps: None,
            rates: vec![2, 4, 11, 22, 12, 18, 24, 36, 48, 72, 96, 108],
            operating_channels: vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14],
        }
    }
}
