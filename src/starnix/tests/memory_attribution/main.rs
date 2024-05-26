// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use diagnostics_reader::{ArchiveReader, Logs};
use fidl_fuchsia_component as fcomponent;
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

    // Start the container.
    let _execution = realm.root.start().await.expect("start test realm");

    // Use the container to start the Starnix program.
    let realm_proxy =
        realm.root.connect_to_protocol_at_exposed_dir::<fcomponent::RealmMarker>().unwrap();
    let factory = ScopedInstanceFactory::new(PROGRAM_COLLECTION).with_realm_proxy(realm_proxy);
    let program = factory.new_instance(PROGRAM_URL).await.unwrap();

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
}
