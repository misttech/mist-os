// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use assert_matches::assert_matches;
use diagnostics_reader::{ArchiveReader, Logs};
use fidl_fuchsia_component as fcomponent;
use fidl_fuchsia_component_decl as fdecl;
use fidl_fuchsia_memory_attribution as fattribution;
use fuchsia_component_test::{RealmBuilder, RealmBuilderParams, ScopedInstanceFactory};
use futures::StreamExt;
use moniker::Moniker;

const PROGRAM_COLLECTION: &str = "debian_programs";

#[fuchsia::test]
async fn mmap_anonymous() {
    const PROGRAM_URL: &str = "mmap_anonymous_then_sleep_package#meta/mmap_anonymous_then_sleep.cm";
    const EXPECTED_LOG: &str = "mmap_anonymous_then_sleep started";

    let builder = RealmBuilder::with_params(
        RealmBuilderParams::new().from_relative_url("#meta/realm.cm").start(false),
    )
    .await
    .expect("created");
    let realm = builder.build().await.expect("build test realm");

    // Start the container and obtain its execution controller.
    let (container_controller, server_end) =
        fidl::endpoints::create_proxy::<fcomponent::ControllerMarker>().unwrap();
    let realm_proxy =
        realm.root.connect_to_protocol_at_exposed_dir::<fcomponent::RealmMarker>().unwrap();
    realm_proxy
        .open_controller(
            &fdecl::ChildRef { name: "debian_container".to_string(), collection: None },
            server_end,
        )
        .await
        .unwrap()
        .expect("open_controller");
    let (container_execution, server_end) =
        fidl::endpoints::create_proxy::<fcomponent::ExecutionControllerMarker>().unwrap();
    container_controller
        .start(fcomponent::StartChildArgs::default(), server_end)
        .await
        .unwrap()
        .expect("start debian container");

    // Use the container to start the Starnix program.
    let factory = ScopedInstanceFactory::new(PROGRAM_COLLECTION).with_realm_proxy(realm_proxy);
    let program = factory.new_instance(PROGRAM_URL).await.unwrap();

    // Connect to the attribution protocol of the starnix runner.
    let attribution_provider =
        realm.root.connect_to_protocol_at_exposed_dir::<fattribution::ProviderMarker>().unwrap();
    let introspector =
        realm.root.connect_to_protocol_at_exposed_dir::<fcomponent::IntrospectorMarker>().unwrap();
    let mut attribution = attribution_testing::attribute_memory(
        "starnix_runner".to_string(),
        attribution_provider,
        introspector,
    );

    // Stream memory attribution data until the container shows up in the reporting.
    let mut tree: attribution_testing::Principal;
    loop {
        tree = attribution.next().await.unwrap();
        if tree.children.len() == 1 {
            break;
        }
    }
    // Starnix runner should report a single container, backed by a job.
    assert_eq!(tree.children.len(), 1);
    assert_eq!(tree.children[0].name, "debian_container");
    assert_eq!(tree.children[0].children.len(), 0);
    assert_eq!(tree.children[0].resources.len(), 1);

    // Wait for magic log from the program.
    let mut reader = ArchiveReader::new();
    let selector: Moniker = realm.root.moniker().parse().unwrap();
    let selector = selectors::sanitize_moniker_for_selectors(&selector.to_string());
    let selector = format!("{}/**:root", selector);
    reader.add_selector(selector);
    let mut logs = reader.snapshot_then_subscribe::<Logs>().unwrap();
    loop {
        let message = logs.next().await.unwrap().unwrap();
        if message.msg().unwrap().contains(EXPECTED_LOG) {
            break;
        }
    }

    // TODO(https://fxbug.dev/337865227): The starnix runner need to report the VMO KOIDs
    // using the `fuchsia.memory.attribution.Provider` protocol.

    drop(program);

    // Stop the container and verify that the starnix runner reports that the
    // container is removed.
    container_execution.stop().unwrap();
    let event = container_execution.take_event_stream().next().await.unwrap().unwrap();
    assert_matches!(event, fcomponent::ExecutionControllerEvent::OnStop { .. });
    loop {
        tree = attribution.next().await.unwrap();
        if tree.children.is_empty() {
            break;
        }
    }
}
