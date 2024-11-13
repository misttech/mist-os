// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use test_realm_helpers::constants::DEFAULT_CLIENT_STA_ADDR;
use wlan_common::ie::fake_ht_capabilities;
use zerocopy::IntoBytes;
use {
    fidl_fuchsia_wlan_common as fidl_common, fidl_fuchsia_wlan_fullmac as fidl_fullmac,
    fidl_fuchsia_wlan_ieee80211 as fidl_ieee80211, fidl_fuchsia_wlan_sme as fidl_sme,
};

/// Contains all the configuration required for the fullmac driver.
/// These are primarily used to respond to SME query requests.
/// By default, the configuration is a client with DEFAULT_CLIENT_STA_ADDR that
/// supports 2.4 GHz bands and HT capabilities.
#[derive(Debug, Clone)]
pub struct FullmacDriverConfig {
    pub query_info: fidl_fullmac::WlanFullmacImplQueryResponse,
    pub mac_sublayer_support: fidl_common::MacSublayerSupport,
    pub security_support: fidl_common::SecuritySupport,
    pub spectrum_management_support: fidl_common::SpectrumManagementSupport,
    pub sme_legacy_privacy_support: fidl_sme::LegacyPrivacySupport,
}

impl Default for FullmacDriverConfig {
    /// By default, the driver is configured as a client.
    fn default() -> Self {
        Self {
            query_info: default_fullmac_query_info(),
            mac_sublayer_support: default_mac_sublayer_support(),
            security_support: default_security_support(),
            spectrum_management_support: default_spectrum_management_support(),
            sme_legacy_privacy_support: default_sme_legacy_privacy_support(),
        }
    }
}

impl FullmacDriverConfig {
    pub fn default_ap() -> Self {
        Self {
            query_info: fidl_fullmac::WlanFullmacImplQueryResponse {
                role: Some(fidl_common::WlanMacRole::Ap),
                ..default_fullmac_query_info()
            },
            ..Default::default()
        }
    }
}

pub fn default_fullmac_query_info() -> fidl_fullmac::WlanFullmacImplQueryResponse {
    fidl_fullmac::WlanFullmacImplQueryResponse {
        sta_addr: Some(DEFAULT_CLIENT_STA_ADDR),
        role: Some(fidl_common::WlanMacRole::Client),
        band_caps: Some(vec![default_fullmac_band_capability()]),
        ..Default::default()
    }
}

pub fn default_mac_sublayer_support() -> fidl_common::MacSublayerSupport {
    fidl_common::MacSublayerSupport {
        rate_selection_offload: fidl_common::RateSelectionOffloadExtension { supported: false },
        data_plane: fidl_common::DataPlaneExtension {
            data_plane_type: fidl_common::DataPlaneType::GenericNetworkDevice,
        },
        device: fidl_common::DeviceExtension {
            is_synthetic: false,
            mac_implementation_type: fidl_common::MacImplementationType::Fullmac,
            tx_status_report_supported: false,
        },
    }
}

pub fn default_security_support() -> fidl_common::SecuritySupport {
    fidl_common::SecuritySupport {
        sae: fidl_common::SaeFeature {
            driver_handler_supported: false,
            sme_handler_supported: true,
        },
        mfp: fidl_common::MfpFeature { supported: true },
    }
}

pub fn default_sme_legacy_privacy_support() -> fidl_sme::LegacyPrivacySupport {
    fidl_sme::LegacyPrivacySupport { wep_supported: false, wpa1_supported: false }
}

fn default_spectrum_management_support() -> fidl_common::SpectrumManagementSupport {
    fidl_common::SpectrumManagementSupport { dfs: fidl_common::DfsFeature { supported: false } }
}

fn default_fullmac_band_capability() -> fidl_fullmac::WlanFullmacBandCapability {
    let mut cap = fidl_fullmac::WlanFullmacBandCapability {
        band: fidl_common::WlanBand::TwoGhz,
        basic_rates: vec![2, 4, 11, 22, 12, 18, 24, 36, 48, 72, 96, 108],
        ht_supported: true,
        ht_caps: fidl_ieee80211::HtCapabilities {
            bytes: fake_ht_capabilities().as_bytes().try_into().unwrap(),
        },
        vht_supported: false,
        vht_caps: fidl_ieee80211::VhtCapabilities { bytes: [0; 12] },
        operating_channel_count: 11,
        operating_channel_list: [0; 256],
    };

    // By default, the fullmac fake driver supports 2 GHz channels in the US.
    // Specifically, channels 12-14 are avoided or not allowed in the US.
    for i in 0..11 {
        cap.operating_channel_list[i] = (i + 1) as u8;
    }
    cap
}
