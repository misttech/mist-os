// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::format_err;

use crate::subsystems::prelude::*;
use assembly_config_capabilities::{Config, ConfigValueType};
use assembly_config_schema::platform_config::bluetooth_config::{
    AudioGatewayConfig, BluetoothConfig, HandsFreeConfig, HfpCodecId, Snoop,
};
use assembly_config_schema::platform_config::media_config::{AudioConfig, PlatformMediaConfig};

pub(crate) struct BluetoothSubsystemConfig;
impl DefineSubsystemConfiguration<(&BluetoothConfig, &PlatformMediaConfig)>
    for BluetoothSubsystemConfig
{
    fn define_configuration(
        context: &ConfigurationContext<'_>,
        config: &(&BluetoothConfig, &PlatformMediaConfig),
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        let (config, media_config) = config;
        // Snoop is only useful when Inspect filtering is turned on. In practice, this is in Eng &
        // UserDebug builds.
        match (context.build_type, config.snoop()) {
            (_, Snoop::None) => {}
            (BuildType::User, _) => return Err(format_err!("Snoop forbidden on user builds")),
            (_, Snoop::Eager) => {
                builder.platform_bundle("bluetooth_snoop_eager");
            }
            (_, Snoop::Lazy) => {
                builder.platform_bundle("bluetooth_snoop_lazy");
            }
        }

        // Include bt-transport-uart driver through a platform AIB.
        if context.board_info.provides_feature("fuchsia::bt_transport_uart")
            && (*context.feature_set_level == FeatureSetLevel::Standard
                || *context.feature_set_level == FeatureSetLevel::Utility)
        {
            builder.platform_bundle("bt_transport_uart_driver");
        }

        let BluetoothConfig::Standard { profiles, core, snoop: _ } = config else {
            return Ok(());
        };

        // Bluetooth Core & Profile packages can only be added to the Standard platform
        // service level.
        if *context.feature_set_level != FeatureSetLevel::Standard {
            return Err(format_err!(
                "Bluetooth core & profiles are forbidden on non-Standard service levels"
            ));
        }
        builder.platform_bundle("bluetooth_core");
        builder.set_config_capability(
            "fuchsia.bluetooth.LegacyPairing",
            Config::new(ConfigValueType::Bool, core.legacy_pairing_enabled.into()),
        )?;

        if profiles.a2dp.enabled {
            builder.platform_bundle("bluetooth_a2dp");
        }
        if profiles.avrcp.enabled {
            builder.platform_bundle("bluetooth_avrcp");
        }
        if profiles.did.enabled {
            builder.platform_bundle("bluetooth_device_id");
            let mut did_config =
                builder.package("bt-device-id").component("meta/bt-device-id.cm")?;
            did_config
                .field("vendor_id", profiles.did.vendor_id)?
                .field("product_id", profiles.did.product_id)?
                .field("version", profiles.did.version)?
                .field("primary", profiles.did.primary)?
                .field(
                    "service_description",
                    profiles.did.service_description.clone().unwrap_or(String::new()),
                )?;
        }

        // TODO(https://fxbug.dev/362573469): Bail if the features don't make sense
        // (VoiceRecognitionText without EnhancedVoiceRecognitionStatus, for example)
        let hfp_supported_codecs = if profiles.hfp.codecs_supported.is_empty() {
            vec![HfpCodecId::Cvsd, HfpCodecId::Msbc, HfpCodecId::Lc3Swb]
        } else {
            profiles.hfp.codecs_supported.clone()
        };
        if let AudioGatewayConfig::Enabled(hfp_ag_features) = &profiles.hfp.audio_gateway {
            builder.platform_bundle("bluetooth_hfp_ag");

            let controller_encodes = if profiles.hfp.controller_encodes.is_empty() {
                vec![HfpCodecId::Cvsd, HfpCodecId::Msbc]
            } else {
                profiles.hfp.controller_encodes.clone()
            };

            let offload_type = match media_config.audio {
                Some(AudioConfig::FullStack(_)) => "dai",
                Some(AudioConfig::DeviceRegistry(_)) => "codec",
                None => return Err(format_err!("Bluetooth HFP requires an audio stack")),
            };

            let mut hfp_ag_config = builder
                .package("bt-hfp-audio-gateway")
                .component("meta/bt-hfp-audio-gateway.cm")?;
            hfp_ag_config
                .field("three_way_calling", hfp_ag_features.three_way_calling)?
                .field("reject_incoming_voice_call", hfp_ag_features.reject_incoming_call)?
                .field("in_band_ringtone", hfp_ag_features.inband_ringtone)?
                .field("voice_recognition", hfp_ag_features.voice_recognition)?
                .field(
                    "echo_canceling_and_noise_reduction",
                    hfp_ag_features.echo_canceling_and_noise_reduction,
                )?
                .field(
                    "attach_phone_number_to_voice_tag",
                    hfp_ag_features.attach_phone_number_voice_tag,
                )?
                .field("enhanced_call_controls", hfp_ag_features.enhanced_call_control)?
                .field(
                    "enhanced_voice_recognition",
                    hfp_ag_features.enhanced_voice_recognition_status,
                )?
                .field(
                    "enhanced_voice_recognition_with_text",
                    hfp_ag_features.voice_recognition_text,
                )?
                .field("controller_encoding_cvsd", controller_encodes.contains(&HfpCodecId::Cvsd))?
                .field("controller_encoding_msbc", controller_encodes.contains(&HfpCodecId::Msbc))?
                .field("wide_band_speech", hfp_supported_codecs.contains(&HfpCodecId::Msbc))?
                .field("offload_type", offload_type)?;
        }
        if let HandsFreeConfig::Enabled(hfp_hf_features) = &profiles.hfp.hands_free {
            builder.platform_bundle("bluetooth_hfp_hf");

            let mut hfp_hf_config =
                builder.package("bt-hfp-hands-free").component("meta/bt-hfp-hands-free.cm")?;
            hfp_hf_config
                .field("ec_or_nr", hfp_hf_features.echo_canceling_and_noise_reduction)?
                .field("call_waiting_or_three_way_calling", hfp_hf_features.three_way_calling)?
                .field("cli_presentation_capability", hfp_hf_features.calling_line_identification)?
                .field("voice_recognition_activation", hfp_hf_features.voice_recognition)?
                .field("remote_volume_control", hfp_hf_features.remote_volume_control)?
                .field("wide_band_speech", hfp_supported_codecs.contains(&HfpCodecId::Msbc))?
                .field("enhanced_voice_recognition", hfp_hf_features.enhanced_voice_recognition)?
                .field(
                    "enhanced_voice_recognition_with_text",
                    hfp_hf_features.voice_recognition_text,
                )?;
        }
        if profiles.map.mce_enabled {
            builder.platform_bundle("bluetooth_map_mce");
        }

        if *context.feature_set_level == FeatureSetLevel::Standard
            && *context.build_type == BuildType::Eng
        {
            builder.platform_bundle("bluetooth_pandora");

            if !profiles.a2dp.enabled {
                if let Some(AudioConfig::FullStack(_)) = media_config.audio {
                    builder.platform_bundle("bluetooth_a2dp_with_consumer");
                }
            }
        }

        Ok(())
    }
}
