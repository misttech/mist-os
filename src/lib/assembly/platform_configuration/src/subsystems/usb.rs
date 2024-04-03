// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::subsystems::prelude::*;
use assembly_config_capabilities::{Config, ConfigNestedValueType, ConfigValueType};
use assembly_config_schema::platform_config::usb_config::*;

pub(crate) struct UsbSubsystemConfig;

impl DefineSubsystemConfiguration<UsbConfig> for UsbSubsystemConfig {
    fn define_configuration(
        _context: &ConfigurationContext<'_>,
        usb: &UsbConfig,
        builder: &mut dyn ConfigurationBuilder,
    ) -> anyhow::Result<()> {
        let usb_peripheral_functions = match &usb.peripheral.functions {
            Some(functions) => functions.iter().map(ToString::to_string).collect::<Vec<String>>(),
            // Setting CDC as the default function if none is configured.
            None => vec![UsbPeripheralFunction::Cdc.to_string()],
        };

        builder.set_config_capability(
            "fuchsia.usb.PeripheralConfig.Functions",
            Config::new(
                ConfigValueType::Vector {
                    nested_type: ConfigNestedValueType::String { max_size: 32 },
                    max_count: 8,
                },
                usb_peripheral_functions.into(),
            ),
        )?;
        Ok(())
    }
}
