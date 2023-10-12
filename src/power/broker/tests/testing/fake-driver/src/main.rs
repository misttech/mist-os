// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Context as _, Error};
use fake_driver_config::Config;
use fidl_fuchsia_power_broker::{
    BinaryPowerLevel, LevelControlMarker, LevelControlProxy, PowerLevel,
};
use fuchsia_component::client::connect_to_protocol;

struct MockPowerElement {
    power_level: PowerLevel,
}

impl MockPowerElement {
    fn get_power_level(&self) -> PowerLevel {
        self.power_level
    }

    fn update_power_level(&mut self, new: PowerLevel) {
        self.power_level = new;
    }
}

#[fuchsia::main]
async fn main() -> Result<(), Error> {
    let mut element = MockPowerElement { power_level: PowerLevel::Binary(BinaryPowerLevel::Off) };
    let config = Config::take_from_startup_handle();

    let client: LevelControlProxy = connect_to_protocol::<LevelControlMarker>()
        .context("failed to connect to LevelControl service")?;

    // Initially, the Power Level is Off.
    element.update_power_level(PowerLevel::Binary(BinaryPowerLevel::Off));
    // Constantly attempt to connect to the Power Broker,
    // updating it with the element's current level, and
    // updating the power level if it receives a new
    // minimum power level from the Power Broker.
    let mut last_required_level: Option<PowerLevel> = None;
    while let Ok(required_level) =
        client.watch_required_level(&config.element_id.clone(), last_required_level.as_ref()).await
    {
        last_required_level = Some(required_level.clone());
        element.update_power_level(required_level);
        let res = client
            .update_current_power_level(&config.element_id.clone(), &element.get_power_level())
            .await?;
        if res.is_err() {
            return Err(anyhow!("UpdateCurrentPowerLevelError: {:?}", res.err()));
        }
    }
    Ok(())
}

#[fuchsia::test]
fn test_mock_power_element_get_power_level() {
    let off = MockPowerElement { power_level: PowerLevel::Binary(BinaryPowerLevel::Off) };
    assert_eq!(off.get_power_level(), PowerLevel::Binary(BinaryPowerLevel::Off));

    let on = MockPowerElement { power_level: PowerLevel::Binary(BinaryPowerLevel::On) };
    assert_eq!(on.get_power_level(), PowerLevel::Binary(BinaryPowerLevel::On));
}

#[fuchsia::test]
fn test_mock_power_element_update_power_level() {
    let mut element = MockPowerElement { power_level: PowerLevel::Binary(BinaryPowerLevel::Off) };
    assert_eq!(element.get_power_level(), PowerLevel::Binary(BinaryPowerLevel::Off));

    element.update_power_level(PowerLevel::Binary(BinaryPowerLevel::On));
    assert_eq!(element.get_power_level(), PowerLevel::Binary(BinaryPowerLevel::On));
}
