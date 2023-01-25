// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// [START example_patches]
use crate::subsystems::prelude::*;
use assembly_config_schema::BuildType;

pub(crate) struct ExampleSubsystemConfig;
impl DefineSubsystemConfiguration<()> for ExampleSubsystemConfig {
    fn define_configuration(
        context: &ConfigurationContext<'_>,
        _subsystem_config: &(),
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        // If the build-type is eng, enabled "foo" for the example component.
        builder
            .package("configured_by_assembly")
            .component("meta/to_configure.cm")?
            .field("enable_foo", matches!(context.build_type, BuildType::Eng))?;

        Ok(())
    }
}
// [END example_patches]
