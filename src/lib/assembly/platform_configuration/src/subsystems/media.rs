// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subsystems::prelude::*;
use crate::util;
use assembly_config_schema::platform_config::media_config::{AudioConfig, PlatformMediaConfig};

pub(crate) struct MediaSubsystem;
impl DefineSubsystemConfiguration<PlatformMediaConfig> for MediaSubsystem {
    fn define_configuration(
        context: &ConfigurationContext<'_>,
        media_config: &PlatformMediaConfig,
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        if *context.feature_set_level == FeatureSupportLevel::Standard
            && *context.build_type == BuildType::Eng
        {
            builder.platform_bundle("audio_development_support");

            if context.board_info.provides_feature("fuchsia::video_encoders") {
                builder.platform_bundle("video_development_support");
            }
        }

        match (&media_config.audio, media_config.audio_device_registry_enabled) {
            (None, false) => {}
            (None, true) => {
                builder.platform_bundle("audio_device_registry");
            }
            (Some(_), true) => {
                anyhow::bail!(
                    "Do not use both media.audio and media.audio_device_registry_enabled"
                );
            }
            (Some(AudioConfig::FullStack(config)), false) => {
                builder.platform_bundle("audio_core_routing");
                if !context.board_info.provides_feature("fuchsia::custom_audio_core") {
                    builder.platform_bundle("audio_core");
                }
                if config.use_adc_device {
                    builder.platform_bundle("audio_core_use_adc_device");
                }
                builder.platform_bundle("soundplayer");
            }
            (Some(AudioConfig::PartialStack), false) => {
                builder.platform_bundle("audio_device_registry");
            }
        }

        if media_config.camera.enabled {
            builder.platform_bundle("camera");
        }

        if let Some(url) = &media_config.multizone_leader.component_url {
            util::add_platform_declared_product_provided_component(
                &url,
                "multizone_leader.core_shard.cml.template",
                context,
                builder,
            )?;

            if *context.build_type == BuildType::Eng {
                builder.core_shard(&context.get_resource("multizone_leader.core_shard_eng.cml"));
            }
        }

        Ok(())
    }
}
