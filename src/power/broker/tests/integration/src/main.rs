// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use fidl_fuchsia_power_broker::{
    BinaryPowerLevel, ControlMarker, LessorMarker, PowerLevel, StatusMarker,
};
use fuchsia_component_test::{Capability, ChildOptions, RealmBuilder, Ref, Route};
use std::{thread, time};

#[fuchsia::test]
async fn test_single_element() -> Result<()> {
    let builder = RealmBuilder::new().await?;

    // Create a Power Broker component
    let power_broker = builder
        .add_child("power_broker", "power-broker#meta/power-broker.cm", ChildOptions::new())
        .await?;

    // Create a MockPowerElement that continuously connects to the PowerBroker
    let fake_driver = builder
        .add_child("fake_driver", "#meta/fake-driver.cm", ChildOptions::new().eager())
        .await?;
    builder.init_mutable_config_to_empty(&fake_driver).await.unwrap();
    const ELEMENT_1: &str = "1";
    builder.set_config_value_string("fake_driver", "element_id", ELEMENT_1).await?;

    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<ControlMarker>())
                .from(&power_broker)
                .to(&fake_driver),
        )
        .await?;

    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<StatusMarker>())
                .from(&power_broker)
                .to(Ref::parent()),
        )
        .await?;

    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<LessorMarker>())
                .from(&power_broker)
                .to(Ref::parent()),
        )
        .await?;

    let realm = builder.build().await?;

    let power_lease = realm.root.connect_to_protocol_at_exposed_dir::<LessorMarker>()?;
    let power_status: fidl_fuchsia_power_broker::StatusProxy =
        realm.root.connect_to_protocol_at_exposed_dir::<StatusMarker>()?;

    // TODO: Replace hardcoded device ID
    // Wait for the fake device to connect
    let initial: time::Instant = time::Instant::now();
    let timeout = time::Duration::from_millis(5000);
    loop {
        let power_level = power_status.get_power_level(ELEMENT_1).await?;
        if power_level == Ok(PowerLevel::Binary(BinaryPowerLevel::Off)) {
            break;
        }
        if time::Instant::now().duration_since(initial) >= timeout {
            assert!(
                false,
                "timed out after waiting {:?} for fake device to connect to Status",
                timeout
            );
        }
        thread::sleep(time::Duration::from_millis(100));
    }

    // acquire lease with PB, device should turn on
    let lease_id = power_lease
        .lease(
            "Test",
            &PowerLevel::Binary(BinaryPowerLevel::On),
            ELEMENT_1,
            &PowerLevel::Binary(BinaryPowerLevel::On),
        )
        .await
        .expect("Lease response not ok");
    let turning_on: time::Instant = time::Instant::now();
    let timeout = time::Duration::from_millis(5000);
    loop {
        let power_level = power_status.get_power_level(ELEMENT_1).await?;
        if power_level == Ok(PowerLevel::Binary(BinaryPowerLevel::On)) {
            break;
        }
        if time::Instant::now().duration_since(turning_on) >= timeout {
            assert!(
                false,
                "timed out after waiting {:?} for fake device to connect to Status",
                timeout
            );
        }
        thread::sleep(time::Duration::from_millis(100));
    }

    // drop lease with PB, device should turn off again
    power_lease.drop_lease(&lease_id).expect("drop failed");
    let turning_off: time::Instant = time::Instant::now();
    let timeout = time::Duration::from_millis(5000);
    loop {
        let power_level = power_status.get_power_level(ELEMENT_1).await?;
        if power_level == Ok(PowerLevel::Binary(BinaryPowerLevel::Off)) {
            break;
        }
        if time::Instant::now().duration_since(turning_off) >= timeout {
            assert!(
                false,
                "timed out after waiting {:?} for fake device to connect to Status",
                timeout
            );
        }
        thread::sleep(time::Duration::from_millis(100));
    }

    Ok(())
}

#[fuchsia::test]
async fn test_multiple_elements() -> Result<()> {
    let builder = RealmBuilder::new().await?;

    // Create a Power Broker component
    let power_broker = builder
        .add_child("power_broker", "power-broker#meta/power-broker.cm", ChildOptions::new())
        .await?;

    // Create two fake elements
    const ELEMENT_1: &str = "1";
    const ELEMENT_2: &str = "2";
    let element_ids = [ELEMENT_1, ELEMENT_2];
    for element_id in element_ids {
        let element_name = format!("fake_element_{}", element_id);
        let fake_element: fuchsia_component_test::ChildRef = builder
            .add_child(element_name.clone(), "#meta/fake-driver.cm", ChildOptions::new().eager())
            .await?;
        builder.init_mutable_config_to_empty(&fake_element).await.unwrap();
        builder.set_config_value_string(element_name.clone(), "element_id", element_id).await?;
        builder
            .add_route(
                Route::new()
                    .capability(Capability::protocol::<ControlMarker>())
                    .from(&power_broker)
                    .to(&fake_element),
            )
            .await?;
    }

    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<StatusMarker>())
                .from(&power_broker)
                .to(Ref::parent()),
        )
        .await?;

    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<LessorMarker>())
                .from(&power_broker)
                .to(Ref::parent()),
        )
        .await?;

    let realm = builder.build().await?;

    let power_lease = realm.root.connect_to_protocol_at_exposed_dir::<LessorMarker>()?;
    let power_status: fidl_fuchsia_power_broker::StatusProxy =
        realm.root.connect_to_protocol_at_exposed_dir::<StatusMarker>()?;

    // Wait for the elements to connect
    let initial: time::Instant = time::Instant::now();
    let timeout = time::Duration::from_millis(5000);
    loop {
        let mut all_connected: bool = true;
        for element_id in element_ids {
            let power_level = power_status.get_power_level(element_id).await?;
            if power_level != Ok(PowerLevel::Binary(BinaryPowerLevel::Off)) {
                all_connected = false;
            }
        }
        if all_connected {
            break;
        }
        if time::Instant::now().duration_since(initial) >= timeout {
            assert!(
                false,
                "timed out after waiting {:?} for fake device to connect to Status",
                timeout
            );
        }
        thread::sleep(time::Duration::from_millis(100));
    }

    // acquire lease for 1 with PB, 1 should turn on
    let lease_id = power_lease
        .lease(
            "Test",
            &PowerLevel::Binary(BinaryPowerLevel::On),
            ELEMENT_1,
            &PowerLevel::Binary(BinaryPowerLevel::On),
        )
        .await
        .expect("Lease response not ok");
    let turning_on: time::Instant = time::Instant::now();
    let timeout = time::Duration::from_millis(5000);
    loop {
        let power_level = power_status.get_power_level(ELEMENT_1).await?;
        if power_level == Ok(PowerLevel::Binary(BinaryPowerLevel::On)) {
            break;
        }
        if time::Instant::now().duration_since(turning_on) >= timeout {
            assert!(false, "timed out after waiting {:?} for fake device to turn ON", timeout);
        }
        thread::sleep(time::Duration::from_millis(100));
    }
    let power_level = power_status.get_power_level(ELEMENT_2).await?;
    assert_eq!(
        power_level,
        Ok(PowerLevel::Binary(BinaryPowerLevel::Off)),
        "Element 2 should still be OFF"
    );

    // drop lease for 1 with PB, 1 should turn off again
    power_lease.drop_lease(&lease_id).expect("drop failed");
    let turning_off: time::Instant = time::Instant::now();
    let timeout = time::Duration::from_millis(5000);
    loop {
        let power_level = power_status.get_power_level(ELEMENT_1).await?;
        if power_level == Ok(PowerLevel::Binary(BinaryPowerLevel::Off)) {
            break;
        }
        if time::Instant::now().duration_since(turning_off) >= timeout {
            assert!(false, "timed out after waiting {:?} for fake device to turn OFF", timeout);
        }
        thread::sleep(time::Duration::from_millis(100));
    }
    let power_level = power_status.get_power_level(ELEMENT_2).await?;
    assert_eq!(
        power_level,
        Ok(PowerLevel::Binary(BinaryPowerLevel::Off)),
        "Element 2 should still be OFF"
    );

    Ok(())
}

#[fuchsia::test]
async fn test_transitive_leases() -> Result<()> {
    let builder = RealmBuilder::new().await?;

    // Create a Power Broker component
    // Topology:
    // A <- B <- C -> D
    // TODO(b/299463665): Assumes hardcoded topology, specify it here instead.
    let power_broker = builder
        .add_child("power_broker", "power-broker#meta/power-broker.cm", ChildOptions::new())
        .await?;

    // Create three fake elements
    const ELEMENT_A: &str = "A";
    const ELEMENT_B: &str = "B";
    const ELEMENT_C: &str = "C";
    let element_ids = [ELEMENT_A, ELEMENT_B, ELEMENT_C];
    for element_id in element_ids {
        let element_name = format!("fake_element_{}", element_id);
        let fake_element: fuchsia_component_test::ChildRef = builder
            .add_child(element_name.clone(), "#meta/fake-driver.cm", ChildOptions::new().eager())
            .await?;
        builder.init_mutable_config_to_empty(&fake_element).await.unwrap();
        builder.set_config_value_string(element_name.clone(), "element_id", element_id).await?;
        builder
            .add_route(
                Route::new()
                    .capability(Capability::protocol::<ControlMarker>())
                    .from(&power_broker)
                    .to(&fake_element),
            )
            .await?;
    }

    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<StatusMarker>())
                .from(&power_broker)
                .to(Ref::parent()),
        )
        .await?;

    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<LessorMarker>())
                .from(&power_broker)
                .to(Ref::parent()),
        )
        .await?;

    let realm = builder.build().await?;

    let power_lease = realm.root.connect_to_protocol_at_exposed_dir::<LessorMarker>()?;
    let power_status: fidl_fuchsia_power_broker::StatusProxy =
        realm.root.connect_to_protocol_at_exposed_dir::<StatusMarker>()?;

    // Wait for the elements to connect
    let initial: time::Instant = time::Instant::now();
    let timeout = time::Duration::from_millis(5000);
    loop {
        let mut all_connected: bool = true;
        for element_id in element_ids {
            let power_level = power_status.get_power_level(element_id).await?;
            if power_level != Ok(PowerLevel::Binary(BinaryPowerLevel::Off)) {
                all_connected = false;
            }
        }
        if all_connected {
            break;
        }
        if time::Instant::now().duration_since(initial) >= timeout {
            assert!(
                false,
                "timed out after waiting {:?} for fake device to connect to Status",
                timeout
            );
        }
        thread::sleep(time::Duration::from_millis(100));
    }

    // acquire lease for B with PB, both A and B should turn on
    // because B has a dependency on A
    let lease_id = power_lease
        .lease(
            "Test",
            &PowerLevel::Binary(BinaryPowerLevel::On),
            ELEMENT_B,
            &PowerLevel::Binary(BinaryPowerLevel::On),
        )
        .await
        .expect("Lease response not ok");
    let turning_on: time::Instant = time::Instant::now();
    let timeout = time::Duration::from_millis(5000);
    loop {
        let mut all_on: bool = true;
        for element_id in [ELEMENT_A, ELEMENT_B] {
            let power_level = power_status.get_power_level(element_id).await?;
            if power_level != Ok(PowerLevel::Binary(BinaryPowerLevel::On)) {
                all_on = false;
            }
        }
        if all_on {
            break;
        }
        if time::Instant::now().duration_since(turning_on) >= timeout {
            assert!(false, "timed out after waiting {:?} for fake device to turn ON", timeout);
        }
        thread::sleep(time::Duration::from_millis(100));
    }
    let power_level = power_status.get_power_level(ELEMENT_C).await?;
    assert_eq!(
        power_level,
        Ok(PowerLevel::Binary(BinaryPowerLevel::Off)),
        "Element C should still be OFF"
    );

    // drop lease for B with PB, both A and B should turn off again
    power_lease.drop_lease(&lease_id).expect("drop failed");
    let turning_off: time::Instant = time::Instant::now();
    let timeout = time::Duration::from_millis(5000);
    loop {
        let mut all_off: bool = true;
        for element_id in [ELEMENT_A, ELEMENT_B] {
            let power_level = power_status.get_power_level(element_id).await?;
            if power_level != Ok(PowerLevel::Binary(BinaryPowerLevel::Off)) {
                all_off = false;
            }
        }
        if all_off {
            break;
        }
        if time::Instant::now().duration_since(turning_off) >= timeout {
            assert!(false, "timed out after waiting {:?} for fake device to turn OFF", timeout);
        }
        thread::sleep(time::Duration::from_millis(100));
    }
    let power_level = power_status.get_power_level(ELEMENT_C).await?;
    assert_eq!(
        power_level,
        Ok(PowerLevel::Binary(BinaryPowerLevel::Off)),
        "Element C should still be OFF"
    );

    Ok(())
}
