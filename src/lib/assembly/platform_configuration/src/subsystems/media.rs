// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subsystems::prelude::*;
use crate::util;
use anyhow::{bail, ensure};
use assembly_config_schema::platform_config::media_config::{AudioConfig, PlatformMediaConfig};

pub(crate) struct MediaSubsystem;
impl DefineSubsystemConfiguration<PlatformMediaConfig> for MediaSubsystem {
    fn define_configuration(
        context: &ConfigurationContext<'_>,
        media_config: &PlatformMediaConfig,
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        if *context.feature_set_level == FeatureSetLevel::Standard
            && *context.build_type == BuildType::Eng
        {
            builder.platform_bundle("audio_development_support");

            if context.board_info.provides_feature("fuchsia::video_encoders") {
                builder.platform_bundle("video_development_support");
            }
            if context.board_info.provides_feature("fuchsia::intel_hda") {
                builder.platform_bundle("intel_hda");
            }
        }

        match &media_config.audio {
            Some(AudioConfig::FullStack(config)) => {
                builder.platform_bundle("audio_core_routing");
                if !context.board_info.provides_feature("fuchsia::custom_audio_core") {
                    builder.platform_bundle("audio_core");
                }
                if config.use_adc_device {
                    builder.platform_bundle("audio_core_use_adc_device");
                }
                builder.platform_bundle("soundplayer");
            }
            Some(AudioConfig::PartialStack) => {
                builder.platform_bundle("audio_device_registry");
                builder.platform_bundle("audio_device_registry_demand");
            }
            Some(AudioConfig::DeviceRegistry(adr_config)) => {
                builder.platform_bundle("audio_device_registry");
                if adr_config.eager_start {
                    builder.platform_bundle("audio_device_registry_eager");
                } else {
                    builder.platform_bundle("audio_device_registry_demand");
                }
            }
            None => {}
        }

        if media_config.camera.enabled {
            builder.platform_bundle("camera");
        }

        if let Some(url) = &media_config.multizone_leader.component_url {
            let Some(AudioConfig::FullStack(_)) = media_config.audio else {
                bail!("multizone_leader requires {{ 'audio': 'full_stack' }}");
            };

            util::add_platform_declared_product_provided_component(
                url,
                "multizone_leader.core_shard.cml.template",
                context,
                builder,
            )?;

            if *context.build_type == BuildType::Eng {
                builder.core_shard(&context.get_resource("multizone_leader.core_shard_eng.cml"));
            }
        }

        if media_config.enable_codecs {
            ensure!(
                *context.feature_set_level == FeatureSetLevel::Standard,
                "Codecs can only be enabled in the 'standard' feature set level."
            );
            builder.platform_bundle("media_codecs");
        }

        if media_config.enable_sessions {
            ensure!(
                *context.feature_set_level == FeatureSetLevel::Standard,
                "Media sessions can only be enabled in the 'standard' feature set level."
            );
            let Some(AudioConfig::FullStack(_)) = media_config.audio else {
                bail!("media.enable_sessions requires {{ 'audio': 'full_stack' }}");
            };

            builder.platform_bundle("media_sessions");
        }

        Ok(())
    }
}
