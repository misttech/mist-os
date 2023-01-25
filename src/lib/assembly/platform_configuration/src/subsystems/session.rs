// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subsystems::prelude::*;
use anyhow::ensure;

pub(crate) struct SessionConfig;
impl DefineSubsystemConfiguration<String> for SessionConfig {
    fn define_configuration(
        _context: &ConfigurationContext<'_>,
        session_url: &String,
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        // Configure the session URL.
        ensure!(
            session_url.is_empty() || session_url.starts_with("fuchsia-pkg://"),
            "valid session URLs must start with `fuchsia-pkg://`, got `{}`",
            session_url
        );
        builder
            .package("session_manager")
            .component("meta/session_manager.cm")?
            .field("session_url", session_url.to_owned())?;

        Ok(())
    }
}
