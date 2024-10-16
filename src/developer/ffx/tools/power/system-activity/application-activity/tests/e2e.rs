// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use diagnostics_data::InspectData;
use ffx_e2e_emu::IsolatedEmulator;

#[fuchsia::test]
async fn taking_lease_adds_lease_to_broker_inspect() {
    let emu = IsolatedEmulator::start("application-activity-test").await.unwrap();

    // Start SAG. "stop" is a no-op since no application-activity lease exists at this point.
    emu.ffx(&["power", "system-activity", "application-activity", "stop"]).await.unwrap();

    std::thread::sleep(std::time::Duration::from_secs(1));
    let lease_count = dbg!(get_active_leases(&emu).await);

    emu.ffx(&["power", "system-activity", "application-activity", "start"]).await.unwrap();

    // Wait until the command exiting results in more leases being held.
    loop {
        if dbg!(get_active_leases(&emu).await).len() > lease_count.len() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }

    emu.stop().await;
}

async fn get_active_leases(emu: &IsolatedEmulator) -> Vec<String> {
    let broker_inspect_json = emu
        .ffx_output(&["--machine", "json", "inspect", "show", "/bootstrap/power-broker"])
        .await
        .unwrap();
    let data: Vec<InspectData> = serde_json::from_str(&broker_inspect_json).unwrap();
    assert_eq!(data.len(), 1, "only one component's inspect should be returned");
    data[0]
        .payload
        .as_ref()
        .unwrap()
        .get_child("broker")
        .unwrap()
        .get_child("leases")
        .unwrap()
        .clone()
        .properties
        .iter()
        .map(|l| l.key().to_string())
        .collect()
}
