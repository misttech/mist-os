// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subsystems::prelude::*;

pub(crate) struct MediaSubsystem;
impl DefineSubsystemConfiguration<()> for MediaSubsystem {
    fn define_configuration(
        context: &ConfigurationContext<'_>,
        _: &(),
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        if *context.feature_set_level == FeatureSupportLevel::Minimal
            && *context.build_type == BuildType::Eng
        {
            builder.platform_bundle("audio_dev_support");

            // TODO(fxbug.dev/126943): Enable 'ffx audio' support here.
        }

        Ok(())
    }
}
