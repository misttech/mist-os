// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use component_events::{events::*, matcher::*, sequence::*};
use diagnostics_assertions::assert_data_tree;
use diagnostics_reader::{ArchiveReader, Inspect};
use fidl_fuchsia_component as fcomponent;
use fidl_fuchsia_component_decl as fdecl;
use fuchsia_component::client;
use fuchsia_component_test::ScopedInstanceFactory;

#[fuchsia::main]
async fn main() {
    let mut reader = ArchiveReader::new();
    reader.add_selector("<component_manager>:root");
    let realm = client::connect_to_protocol::<fcomponent::RealmMarker>()
        .expect("could not connect to Realm service");

    let factory = ScopedInstanceFactory::new("coll");
    let instance = factory
        .new_named_instance("parent", "#meta/parent.cm")
        .await
        .expect("create scoped instance");
    let mut event_stream = component_events::events::EventStream::open().await.unwrap();
    let _ = instance.connect_to_binder().unwrap();
    let _ = EventMatcher::ok()
        .moniker("./coll:parent")
        .wait::<Started>(&mut event_stream)
        .await
        .unwrap();
    let _ = EventMatcher::ok()
        .moniker("./coll:parent/child")
        .wait::<Started>(&mut event_stream)
        .await
        .unwrap();

    let data = reader.snapshot::<Inspect>().await.expect("got inspect data");
    assert_data_tree!(data[0].payload.as_ref().unwrap(), root: contains {
        stats: contains {
            measurements: contains {
                component_count: 4u64,
                task_count: 4u64,
                components: {
                    "<component_manager>": contains {},
                    "root/archivist": contains {},
                    "root/cleanup": contains {},
                    "root/cleanup/coll:parent/child": contains {}
                }
            }
        }
    });

    let event_stream = EventStream::open().await.expect("conenct to event source");

    // Destroy the parent component.
    let child_ref =
        fdecl::ChildRef { name: "parent".to_string(), collection: Some("coll".to_string()) };
    realm
        .destroy_child(&child_ref)
        .await
        .expect("destroy_child failed")
        .expect("failed to destroy child");

    EventSequence::new()
        .all_of(
            vec![
                EventMatcher::default().r#type(Destroyed::TYPE).moniker("./coll:parent/child"),
                EventMatcher::default().r#type(Destroyed::TYPE).moniker("./coll:parent"),
            ],
            Ordering::Unordered,
        )
        .expect(event_stream)
        .await
        .unwrap();

    // We should no longer see the `cleanup/coll:parent` component. It was a component with no diagnostics
    // associated, so it's cleaned up. Retry until we see this.
    let data = reader.snapshot::<Inspect>().await.expect("got inspect data");
    assert_data_tree!(data[0].payload.as_ref().unwrap(), root: contains {
        stats: contains {
            measurements: contains {
                component_count: 4u64,
                task_count: 4u64,
                components: {
                    "<component_manager>": contains {},
                    "root/archivist": contains {},
                    "root/cleanup": contains {},
                    "root/cleanup/coll:parent/child": contains {}
                }
            }
        }
    });
}
