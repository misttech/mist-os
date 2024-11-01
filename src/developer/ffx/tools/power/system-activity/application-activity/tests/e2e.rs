// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use diagnostics_data::InspectData;
use ffx_e2e_emu::IsolatedEmulator;

#[fuchsia::test]
async fn taking_lease_adds_lease_to_broker_inspect() {
    let emu = IsolatedEmulator::start("application-activity-test").await.unwrap();

    emu.ffx(&["power", "system-activity", "application-activity", "start"]).await.unwrap();

    // Wait until application_activity level changing to active.
    loop {
        if get_application_activity_level(&emu).await == 1 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }

    emu.stop().await;
}

async fn get_application_activity_level(emu: &IsolatedEmulator) -> i64 {
    let sag_inspect_json = emu
        .ffx_output(&[
            "--machine",
            "json",
            "inspect",
            "show",
            "/bootstrap/system-activity-governor",
        ])
        .await
        .unwrap();
    let data: Vec<InspectData> = serde_json::from_str(&sag_inspect_json).unwrap();
    assert_eq!(data.len(), 1, "only one component's inspect should be returned");
    data[0]
        .payload
        .as_ref()
        .unwrap()
        .get_child("power_elements")
        .unwrap()
        .get_child("application_activity")
        .unwrap()
        .get_property("power_level")
        .unwrap()
        .clone()
        .number_as_int()
        .unwrap()
}
