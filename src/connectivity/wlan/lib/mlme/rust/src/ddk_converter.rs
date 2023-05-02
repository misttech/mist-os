// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{format_err, Error},
    banjo_fuchsia_hardware_wlan_associnfo as banjo_wlan_associnfo,
    banjo_fuchsia_wlan_common as banjo_common, banjo_fuchsia_wlan_ieee80211 as banjo_ieee80211,
    banjo_fuchsia_wlan_softmac as banjo_wlan_softmac, fidl_fuchsia_wlan_common as fidl_common,
    fidl_fuchsia_wlan_ieee80211 as fidl_ieee80211, fidl_fuchsia_wlan_mlme as fidl_mlme,
    ieee80211::Bssid,
    wlan_common::{capabilities::StaCapabilities, ie, mac::Aid},
    zerocopy::AsBytes,
};

#[macro_export]
macro_rules! banjo_list_to_slice {
    ($banjo_struct:expr, $field_prefix:ident $(,)?) => {{
        use paste::paste;
        paste! {
            unsafe {
                std::slice::from_raw_parts(
                    $banjo_struct.[<$field_prefix _list>],
                    $banjo_struct.[<$field_prefix _count>],
                )
            }
        }
    }};
}

#[macro_export]
macro_rules! banjo_buffer_to_slice {
    ($banjo_struct:expr, $field_prefix:ident $(,)?) => {{
        use paste::paste;
        paste! {
            unsafe {
                std::slice::from_raw_parts(
                    $banjo_struct.[<$field_prefix _buffer>],
                    $banjo_struct.[<$field_prefix _size>],
                )
            }
        }
    }};
}

#[macro_export]
macro_rules! zeroed_array_from_prefix {
    ($slice:expr, $size:expr $(,)?) => {{
        assert!($slice.len() <= $size);
        let mut a = [0; $size];
        a[..$slice.len()].clone_from_slice(&$slice);
        a
    }};
}

pub fn ddk_channel_from_fidl(
    fc: fidl_common::WlanChannel,
) -> Result<banjo_common::WlanChannel, Error> {
    let cbw = match fc.cbw {
        fidl_common::ChannelBandwidth::Cbw20 => banjo_common::ChannelBandwidth::CBW20,
        fidl_common::ChannelBandwidth::Cbw40 => banjo_common::ChannelBandwidth::CBW40,
        fidl_common::ChannelBandwidth::Cbw40Below => banjo_common::ChannelBandwidth::CBW40BELOW,
        fidl_common::ChannelBandwidth::Cbw80 => banjo_common::ChannelBandwidth::CBW80,
        fidl_common::ChannelBandwidth::Cbw160 => banjo_common::ChannelBandwidth::CBW160,
        fidl_common::ChannelBandwidth::Cbw80P80 => banjo_common::ChannelBandwidth::CBW80P80,
        fidl_common::ChannelBandwidthUnknown!() => {
            return Err(format_err!("Unknown channel bandwidth {:?}", fc.cbw));
        }
    };
    Ok(banjo_common::WlanChannel { primary: fc.primary, cbw, secondary80: fc.secondary80 })
}

pub fn build_ddk_assoc_cfg(
    bssid: Bssid,
    aid: Aid,
    channel: banjo_common::WlanChannel,
    negotiated_capabilities: StaCapabilities,
    ht_op: Option<[u8; fidl_ieee80211::HT_OP_LEN as usize]>,
    vht_op: Option<[u8; fidl_ieee80211::VHT_OP_LEN as usize]>,
) -> banjo_wlan_softmac::WlanAssociationConfig {
    let mut rates = [0; banjo_fuchsia_wlan_softmac::WLAN_MAC_MAX_RATES as usize];
    rates[..negotiated_capabilities.rates.len()]
        .clone_from_slice(negotiated_capabilities.rates.as_bytes());
    let ht_cap_is_valid = negotiated_capabilities.ht_cap.is_some();
    let vht_cap_is_valid = negotiated_capabilities.vht_cap.is_some();
    let mut ht_cap =
        banjo_ieee80211::HtCapabilities { bytes: [0u8; fidl_ieee80211::HT_CAP_LEN as usize] };
    let mut vht_cap =
        banjo_ieee80211::VhtCapabilities { bytes: [0u8; fidl_ieee80211::VHT_CAP_LEN as usize] };
    negotiated_capabilities
        .ht_cap
        .map(|negotiated_ht_cap| ht_cap.bytes.copy_from_slice(&negotiated_ht_cap.as_bytes()[..]));
    negotiated_capabilities.vht_cap.map(|negotiated_vht_cap| {
        vht_cap.bytes.copy_from_slice(&negotiated_vht_cap.as_bytes()[..])
    });
    let ht_op_bytes = ht_op.unwrap_or([0; fidl_ieee80211::HT_OP_LEN as usize]);
    let vht_op_bytes = vht_op.unwrap_or([0; fidl_ieee80211::VHT_OP_LEN as usize]);
    banjo_wlan_softmac::WlanAssociationConfig {
        bssid: bssid.0,
        aid,
        // In the association request we sent out earlier, listen_interval is always set to 0,
        // indicating the client never enters power save mode.
        // TODO(fxbug.dev/42217): ath10k disregard this value and hard code it to 1.
        // It is working now but we may need to revisit.
        listen_interval: 0,
        channel,
        // TODO(fxbug.dev/29325): QoS works with Aruba/Ubiquiti for BlockAck session but it may need to be
        // dynamically determined for each outgoing data frame.
        // TODO(fxbug.dev/43938): Derive QoS flag and WMM parameters from device info
        qos: ht_cap_is_valid,
        wmm_params: blank_wmm_params(),

        rates_cnt: negotiated_capabilities.rates.len() as u16, // will not overflow as MAX_RATES_LEN is u8
        rates,
        capability_info: negotiated_capabilities.capability_info.raw(),
        // All the unwrap are safe because the size of the byte array follow wire format.
        ht_cap_is_valid,
        ht_cap,
        ht_op_is_valid: ht_op.is_some(),
        ht_op: { *ie::parse_ht_operation(&ht_op_bytes[..]).unwrap() }.into(),
        vht_cap_is_valid,
        vht_cap,
        vht_op_is_valid: vht_op.is_some(),
        vht_op: { *ie::parse_vht_operation(&vht_op_bytes[..]).unwrap() }.into(),
    }
}

pub fn get_rssi_dbm(rx_info: banjo_wlan_softmac::WlanRxInfo) -> Option<i8> {
    match rx_info.valid_fields & banjo_wlan_softmac::WlanRxInfoValid::RSSI
        != banjo_wlan_softmac::WlanRxInfoValid(0)
        && rx_info.rssi_dbm != 0
    {
        true => Some(rx_info.rssi_dbm),
        false => None,
    }
}

pub fn blank_wmm_params() -> banjo_wlan_associnfo::WlanWmmParameters {
    banjo_wlan_associnfo::WlanWmmParameters {
        apsd: false,
        ac_be_params: blank_wmm_ac_params(),
        ac_bk_params: blank_wmm_ac_params(),
        ac_vi_params: blank_wmm_ac_params(),
        ac_vo_params: blank_wmm_ac_params(),
    }
}

fn blank_wmm_ac_params() -> banjo_wlan_associnfo::WlanWmmAcParams {
    banjo_wlan_associnfo::WlanWmmAcParams {
        ecw_min: 0,
        ecw_max: 0,
        aifsn: 0,
        txop_limit: 0,
        acm: false,
    }
}

pub fn convert_ddk_discovery_support(
    support: banjo_common::DiscoverySupport,
) -> Result<fidl_common::DiscoverySupport, Error> {
    let scan_offload = fidl_common::ScanOffloadExtension {
        supported: support.scan_offload.supported,
        scan_cancel_supported: support.scan_offload.scan_cancel_supported,
    };
    let probe_response_offload = fidl_common::ProbeResponseOffloadExtension {
        supported: support.probe_response_offload.supported,
    };
    Ok(fidl_common::DiscoverySupport { scan_offload, probe_response_offload })
}

pub fn convert_ddk_mac_sublayer_support(
    support: banjo_common::MacSublayerSupport,
) -> Result<fidl_common::MacSublayerSupport, Error> {
    let rate_selection_offload = fidl_common::RateSelectionOffloadExtension {
        supported: support.rate_selection_offload.supported,
    };
    let mac_implementation_type = match support.device.mac_implementation_type {
        banjo_common::MacImplementationType::SOFTMAC => fidl_common::MacImplementationType::Softmac,
        banjo_common::MacImplementationType::FULLMAC => fidl_common::MacImplementationType::Fullmac,
        _ => {
            return Err(format_err!(
                "Unexpected banjo_comon::MacImplementationType value {}",
                support.device.mac_implementation_type.0
            ))
        }
    };
    let device = fidl_common::DeviceExtension {
        is_synthetic: support.device.is_synthetic,
        tx_status_report_supported: support.device.tx_status_report_supported,
        mac_implementation_type: mac_implementation_type,
    };

    let data_plane_type = match support.data_plane.data_plane_type {
        banjo_common::DataPlaneType::ETHERNET_DEVICE => fidl_common::DataPlaneType::EthernetDevice,
        banjo_common::DataPlaneType::GENERIC_NETWORK_DEVICE => {
            fidl_common::DataPlaneType::GenericNetworkDevice
        }
        _ => {
            return Err(format_err!(
                "Unexpected banjo_comon::DataPlaneType value {}",
                support.data_plane.data_plane_type.0
            ))
        }
    };
    let data_plane = fidl_common::DataPlaneExtension { data_plane_type: data_plane_type };
    Ok(fidl_common::MacSublayerSupport { rate_selection_offload, data_plane, device })
}

pub fn convert_ddk_security_support(
    support: banjo_common::SecuritySupport,
) -> Result<fidl_common::SecuritySupport, Error> {
    let mfp = fidl_common::MfpFeature { supported: support.mfp.supported };
    let sae = fidl_common::SaeFeature {
        driver_handler_supported: support.sae.driver_handler_supported,
        sme_handler_supported: support.sae.sme_handler_supported,
    };
    Ok(fidl_common::SecuritySupport { sae, mfp })
}

pub fn convert_ddk_spectrum_management_support(
    support: banjo_common::SpectrumManagementSupport,
) -> Result<fidl_common::SpectrumManagementSupport, Error> {
    Ok(fidl_common::SpectrumManagementSupport {
        dfs: fidl_common::DfsFeature { supported: support.dfs.supported },
    })
}

pub fn convert_ddk_band_cap(
    band_cap: &banjo_wlan_softmac::WlanSoftmacBandCapability,
) -> Result<fidl_mlme::BandCapability, Error> {
    let band = match band_cap.band {
        banjo_common::WlanBand::TWO_GHZ => fidl_common::WlanBand::TwoGhz,
        banjo_common::WlanBand::FIVE_GHZ => fidl_common::WlanBand::FiveGhz,
        _ => return Err(format_err!("Unexpected banjo_comon::WlanBand value {}", band_cap.band.0)),
    };
    let basic_rates = band_cap.basic_rate_list[..band_cap.basic_rate_count as usize].to_vec();
    let operating_channels =
        band_cap.operating_channel_list[..band_cap.operating_channel_count as usize].to_vec();
    let ht_cap = if band_cap.ht_supported {
        Some(Box::new(fidl_ieee80211::HtCapabilities { bytes: band_cap.ht_caps.bytes }))
    } else {
        None
    };
    let vht_cap = if band_cap.vht_supported {
        Some(Box::new(fidl_ieee80211::VhtCapabilities { bytes: band_cap.vht_caps.bytes }))
    } else {
        None
    };
    Ok(fidl_mlme::BandCapability { band, basic_rates, operating_channels, ht_cap, vht_cap })
}

pub fn cssid_from_ssid_unchecked(ssid: &Vec<u8>) -> banjo_ieee80211::CSsid {
    let mut cssid = banjo_ieee80211::CSsid {
        len: ssid.len() as u8,
        data: [0; fidl_ieee80211::MAX_SSID_BYTE_LEN as usize],
    };
    // Ssid never exceeds fidl_ieee80211::MAX_SSID_BYTE_LEN bytes, so this assignment will never panic
    cssid.data[..ssid.len()].copy_from_slice(&ssid[..]);
    cssid
}

#[cfg(test)]
pub mod test_utils {
    use super::*;

    pub fn band_capability_eq(
        left: &banjo_wlan_softmac::WlanSoftmacBandCapability,
        right: &banjo_wlan_softmac::WlanSoftmacBandCapability,
    ) -> bool {
        left.band == right.band
            && left.basic_rate_count == right.basic_rate_count
            && left.basic_rate_list == right.basic_rate_list
            && left.ht_supported == right.ht_supported
            && left.ht_caps == right.ht_caps
            && left.vht_supported == right.vht_supported
            && left.vht_caps == right.vht_caps
            && left.operating_channel_count == right.operating_channel_count
            && left.operating_channel_list == right.operating_channel_list
    }
}

#[cfg(test)]
mod tests {
    use {
        super::*,
        crate::device::{
            fake_discovery_support, fake_mac_sublayer_support, fake_security_support,
            fake_spectrum_management_support, QueryResponse,
        },
        banjo_fuchsia_wlan_ieee80211 as banjo_80211,
        std::convert::TryInto,
        wlan_common::{ie, mac},
        zerocopy::AsBytes,
    };

    #[test]
    fn conversion_from_banjo_list_to_slice_successful() {
        let channels = vec![3, 4, 5];
        let banjo = banjo_wlan_softmac::WlanSoftmacStartPassiveScanRequest {
            channels_list: channels.as_ptr(),
            channels_count: channels.len(),
            min_channel_time: 0,
            max_channel_time: 100,
            min_home_time: 5,
        };
        assert_eq!(&channels, banjo_list_to_slice!(banjo, channels));
    }

    #[test]
    fn conversion_from_banjo_buffer_to_slice_successful() {
        let ssids_list = vec![];
        let channels = vec![];
        let mac_header = vec![0xAA, 0xAA];
        let ies = vec![0xBB, 0xBB, 0xBB];
        let banjo = banjo_wlan_softmac::WlanSoftmacStartActiveScanRequest {
            min_channel_time: 0,
            max_channel_time: 100,
            min_home_time: 5,
            min_probes_per_channel: 1,
            max_probes_per_channel: 6,
            ssids_list: ssids_list.as_ptr(),
            ssids_count: ssids_list.len(),
            channels_list: channels.as_ptr(),
            channels_count: channels.len(),
            mac_header_buffer: mac_header.as_ptr(),
            mac_header_size: mac_header.len(),
            ies_buffer: ies.as_ptr(),
            ies_size: ies.len(),
        };
        assert_eq!(&mac_header, banjo_buffer_to_slice!(banjo, mac_header));
        assert_eq!(&ies, banjo_buffer_to_slice!(banjo, ies));
    }

    #[test]
    fn assoc_cfg_construction_successful() {
        let ddk = build_ddk_assoc_cfg(
            Bssid([1, 2, 3, 4, 5, 6]),
            42,
            banjo_common::WlanChannel {
                primary: 149,
                cbw: banjo_common::ChannelBandwidth::CBW40,
                secondary80: 42,
            },
            StaCapabilities {
                capability_info: mac::CapabilityInfo(0x1234),
                rates: vec![111, 112, 113, 114, 115, 116, 117, 118, 119, 120]
                    .into_iter()
                    .map(ie::SupportedRate)
                    .collect(),
                ht_cap: Some(ie::fake_ht_capabilities()),
                vht_cap: Some(ie::fake_vht_capabilities()),
            },
            Some(ie::fake_ht_operation().as_bytes().try_into().unwrap()),
            Some(ie::fake_vht_operation().as_bytes().try_into().unwrap()),
        );
        assert_eq!([1, 2, 3, 4, 5, 6], ddk.bssid);
        assert_eq!(42, ddk.aid);
        assert_eq!(0, ddk.listen_interval);
        assert_eq!(
            banjo_common::WlanChannel {
                primary: 149,
                cbw: banjo_common::ChannelBandwidth::CBW40,
                secondary80: 42
            },
            ddk.channel
        );
        assert_eq!(true, ddk.qos);

        assert_eq!(10, ddk.rates_cnt);
        assert_eq!([111, 112, 113, 114, 115, 116, 117, 118, 119, 120], ddk.rates[0..10]);
        assert_eq!(&[0; 253][..], &ddk.rates[10..]);

        assert_eq!(0x1234, ddk.capability_info);

        assert_eq!(true, ddk.ht_cap_is_valid);
        assert_eq!(&ie::fake_ht_capabilities().as_bytes()[..], &ddk.ht_cap.bytes[..]);

        assert_eq!(true, ddk.ht_op_is_valid);
        let expected_ht_op: banjo_wlan_softmac::WlanHtOp = ie::fake_ht_operation().into();
        assert_eq!(expected_ht_op, ddk.ht_op);

        assert_eq!(true, ddk.vht_cap_is_valid);
        let expected_vht_cap: banjo_80211::VhtCapabilities = ie::fake_vht_capabilities().into();
        assert_eq!(expected_vht_cap, ddk.vht_cap);

        assert_eq!(true, ddk.vht_op_is_valid);
        let expected_vht_op: banjo_wlan_softmac::WlanVhtOp = ie::fake_vht_operation().into();
        assert_eq!(expected_vht_op, ddk.vht_op);
    }

    fn empty_rx_info() -> banjo_wlan_softmac::WlanRxInfo {
        banjo_wlan_softmac::WlanRxInfo {
            rx_flags: banjo_wlan_softmac::WlanRxInfoFlags(0),
            valid_fields: banjo_wlan_softmac::WlanRxInfoValid(0),
            phy: banjo_common::WlanPhyType::DSSS,
            data_rate: 0,
            channel: banjo_common::WlanChannel {
                primary: 0,
                cbw: banjo_common::ChannelBandwidth::CBW20,
                secondary80: 0,
            },
            mcs: 0,
            rssi_dbm: 0,
            snr_dbh: 0,
        }
    }

    #[test]
    fn test_get_rssi_dbm_field_not_valid() {
        let rx_info = banjo_wlan_softmac::WlanRxInfo {
            valid_fields: banjo_wlan_softmac::WlanRxInfoValid(0),
            rssi_dbm: 20,
            ..empty_rx_info()
        };
        assert_eq!(get_rssi_dbm(rx_info), None);
    }

    #[test]
    fn test_get_rssi_dbm_zero_dbm() {
        let rx_info = banjo_wlan_softmac::WlanRxInfo {
            valid_fields: banjo_wlan_softmac::WlanRxInfoValid::RSSI,
            rssi_dbm: 0,
            ..empty_rx_info()
        };
        assert_eq!(get_rssi_dbm(rx_info), None);
    }

    #[test]
    fn test_get_rssi_dbm_all_good() {
        let rx_info = banjo_wlan_softmac::WlanRxInfo {
            valid_fields: banjo_wlan_softmac::WlanRxInfoValid::RSSI,
            rssi_dbm: 20,
            ..empty_rx_info()
        };
        assert_eq!(get_rssi_dbm(rx_info), Some(20));
    }

    #[test]
    fn test_convert_band_cap() {
        let banjo_band_cap = banjo_wlan_softmac::WlanSoftmacBandCapability {
            band: banjo_common::WlanBand::TWO_GHZ,
            basic_rate_list: zeroed_array_from_prefix!(
                [0x02, 0x04, 0x0b, 0x16, 0x0c, 0x12, 0x18, 0x24, 0x30, 0x48, 0x60, 0x6c],
                banjo_ieee80211::MAX_SUPPORTED_BASIC_RATES as usize
            ),
            basic_rate_count: 12,
            operating_channel_list: zeroed_array_from_prefix!(
                [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14],
                banjo_ieee80211::MAX_UNIQUE_CHANNEL_NUMBERS as usize,
            ),
            operating_channel_count: 14,
            ht_supported: true,
            ht_caps: banjo_ieee80211::HtCapabilities {
                bytes: [
                    0x63, 0x00, // HT capability info
                    0x17, // AMPDU params
                    0xff, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
                    0x00, // Rx MCS bitmask, Supported MCS values: 0-7
                    0x01, 0x00, 0x00, 0x00, // Tx parameters
                    0x00, 0x00, // HT extended capabilities
                    0x00, 0x00, 0x00, 0x00, // TX beamforming capabilities
                    0x00, // ASEL capabilities
                ],
            },
            vht_supported: false,
            vht_caps: banjo_ieee80211::VhtCapabilities { bytes: Default::default() },
        };
        let fidl_band_cap =
            convert_ddk_band_cap(&banjo_band_cap).expect("failed to convert band capability");
        assert_eq!(fidl_band_cap.band, fidl_common::WlanBand::TwoGhz);
        assert_eq!(
            fidl_band_cap.basic_rates,
            vec![0x02, 0x04, 0x0b, 0x16, 0x0c, 0x12, 0x18, 0x24, 0x30, 0x48, 0x60, 0x6c]
        );
        assert_eq!(
            fidl_band_cap.operating_channels,
            vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14]
        );
        assert!(fidl_band_cap.ht_cap.is_some());
        assert!(fidl_band_cap.vht_cap.is_none());
    }

    #[test]
    fn test_convert_device_info() {
        let fake_query_response = QueryResponse::fake();
        let device_info: fidl_mlme::DeviceInfo =
            fake_query_response.try_into().expect("Failed to conver wlan-softmac info");
        assert_eq!(device_info.sta_addr, [7u8; 6]);
        assert_eq!(device_info.role, fidl_common::WlanMacRole::Client);
        assert_eq!(device_info.bands.len(), 2);
    }

    #[test]
    fn test_convert_device_info_unknown_role() {
        let fake_query_response =
            QueryResponse::fake().with_mac_role(banjo_common::WlanMacRole(10));
        fidl_mlme::DeviceInfo::try_from(fake_query_response)
            .expect_err("Shouldn't convert invalid mac role");
    }

    #[test]
    fn test_convert_ddk_discovery_support() {
        let support_ddk = fake_discovery_support();
        let support_fidl = convert_ddk_discovery_support(support_ddk)
            .expect("Failed to convert discovery support");
        assert_eq!(support_fidl.scan_offload.supported, support_ddk.scan_offload.supported);
        assert_eq!(
            support_fidl.probe_response_offload.supported,
            support_ddk.probe_response_offload.supported
        );
    }

    #[test]
    fn test_convert_ddk_mac_sublayer_support() {
        let support_ddk = fake_mac_sublayer_support();
        let support_fidl = convert_ddk_mac_sublayer_support(support_ddk)
            .expect("Failed to convert MAC sublayer support");
        assert_eq!(
            support_fidl.rate_selection_offload.supported,
            support_ddk.rate_selection_offload.supported
        );
        assert_eq!(
            support_fidl.data_plane.data_plane_type,
            fidl_common::DataPlaneType::EthernetDevice
        );
        assert_eq!(support_fidl.device.is_synthetic, support_ddk.device.is_synthetic);
        assert_eq!(
            support_fidl.device.mac_implementation_type,
            fidl_common::MacImplementationType::Softmac
        );
        assert_eq!(
            support_fidl.device.tx_status_report_supported,
            support_ddk.device.tx_status_report_supported
        );
    }

    #[test]
    fn test_convert_ddk_security_support() {
        let support_ddk = fake_security_support();
        let support_fidl =
            convert_ddk_security_support(support_ddk).expect("Failed to convert security support");
        assert_eq!(
            support_fidl.sae.driver_handler_supported,
            support_ddk.sae.driver_handler_supported
        );
        assert_eq!(support_fidl.sae.sme_handler_supported, support_ddk.sae.sme_handler_supported);
        assert_eq!(support_fidl.mfp.supported, support_ddk.mfp.supported);
    }

    #[test]
    fn test_convert_ddk_spectrum_management_support() {
        let support_ddk = fake_spectrum_management_support();
        let support_fidl = convert_ddk_spectrum_management_support(support_ddk)
            .expect("Failed to convert spectrum management support");
        assert_eq!(support_fidl.dfs.supported, support_ddk.dfs.supported);
    }
}
