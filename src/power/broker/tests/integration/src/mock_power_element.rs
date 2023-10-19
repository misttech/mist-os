// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{anyhow, Error};
use fidl_fuchsia_power_broker::{BinaryPowerLevel, LevelControlProxy, PowerLevel};
use futures::channel::mpsc::UnboundedSender;

pub struct MockPowerElement {
    pub power_level: PowerLevel,
    update_channel: Option<UnboundedSender<PowerLevel>>,
}

impl MockPowerElement {
    pub fn new(initial_power_level: PowerLevel) -> Self {
        MockPowerElement { power_level: initial_power_level, update_channel: None }
    }

    pub fn get_power_level(&self) -> PowerLevel {
        self.power_level
    }

    pub fn update_power_level(&mut self, new: PowerLevel) {
        self.power_level = new;
    }

    /// Set the channel that will be sent PowerLevel updates, after calling
    /// UpdateCurrentPowerLevel.
    pub fn set_update_channel(&mut self, channel: Option<UnboundedSender<PowerLevel>>) {
        self.update_channel = channel;
    }

    pub async fn connect(
        &mut self,
        client: LevelControlProxy,
        element_id: &str,
    ) -> Result<(), Error> {
        // Constantly attempt to connect to the Power Broker,
        // updating it with the element's current level, and
        // updating the power level if it receives a new
        // minimum power level from the Power Broker.
        tracing::info!("watching required level...");
        let mut last_required_level: Option<PowerLevel> = None;
        loop {
            let required_level = client
                .watch_required_level(element_id, last_required_level.as_ref())
                .await
                .expect("watch_required_level failed");
            tracing::info!("new required level: {:?}", &required_level);
            last_required_level = Some(required_level.clone());
            self.update_power_level(required_level);
            let res =
                client.update_current_power_level(&element_id, &self.get_power_level()).await?;
            if let Some(channel) = &self.update_channel {
                channel.unbounded_send(self.get_power_level()).expect("send_failed");
            }
            if res.is_err() {
                return Err(anyhow!("UpdateCurrentPowerLevelError: {:?}", res.err()));
            }
        }
    }
}

#[fuchsia::test]
fn test_get_power_level() {
    let off = MockPowerElement::new(PowerLevel::Binary(BinaryPowerLevel::Off));
    assert_eq!(off.get_power_level(), PowerLevel::Binary(BinaryPowerLevel::Off));

    let on = MockPowerElement::new(PowerLevel::Binary(BinaryPowerLevel::On));
    assert_eq!(on.get_power_level(), PowerLevel::Binary(BinaryPowerLevel::On));
}

#[fuchsia::test]
fn test_update_power_level() {
    let mut element = MockPowerElement::new(PowerLevel::Binary(BinaryPowerLevel::Off));
    assert_eq!(element.get_power_level(), PowerLevel::Binary(BinaryPowerLevel::Off));

    element.update_power_level(PowerLevel::Binary(BinaryPowerLevel::On));
    assert_eq!(element.get_power_level(), PowerLevel::Binary(BinaryPowerLevel::On));
}
