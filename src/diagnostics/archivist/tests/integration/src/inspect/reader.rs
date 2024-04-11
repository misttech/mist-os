// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::test_topology;
use anyhow::Error;
use archivist_lib::constants;
use diagnostics_assertions::{assert_data_tree, assert_json_diff, AnyProperty};
use diagnostics_reader::{ArchiveReader, Inspect};
use difference::assert_diff;
use fidl_fuchsia_archivist_test as ftest;
use fidl_fuchsia_diagnostics::{ArchiveAccessorMarker, ArchiveAccessorProxy};
use lazy_static::lazy_static;
use realm_proxy_client::RealmProxyClient;

const MONIKER_KEY: &str = "moniker";
const METADATA_KEY: &str = "metadata";
const TIMESTAMP_KEY: &str = "timestamp";
const TEST_ARCHIVIST_MONIKER: &str = "archivist";

lazy_static! {
    static ref EMPTY_RESULT_GOLDEN: &'static str =
        include_str!("../../test_data/empty_result_golden.json");
    static ref UNIFIED_SINGLE_VALUE_GOLDEN: &'static str =
        include_str!("../../test_data/unified_reader_single_value_golden.json");
    static ref UNIFIED_ALL_GOLDEN: &'static str =
        include_str!("../../test_data/unified_reader_all_golden.json");
    static ref UNIFIED_FULL_FILTER_GOLDEN: &'static str =
        include_str!("../../test_data/unified_reader_full_filter_golden.json");
    static ref PIPELINE_SINGLE_VALUE_GOLDEN: &'static str =
        include_str!("../../test_data/pipeline_reader_single_value_golden.json");
    static ref PIPELINE_ALL_GOLDEN: &'static str =
        include_str!("../../test_data/pipeline_reader_all_golden.json");
    static ref PIPELINE_NONOVERLAPPING_SELECTORS_GOLDEN: &'static str =
        include_str!("../../test_data/pipeline_reader_nonoverlapping_selectors_golden.json");
}

#[fuchsia::test]
async fn read_components_inspect() {
    let realm_proxy = test_topology::create_realm(&ftest::RealmOptions {
        puppets: Some(vec![test_topology::PuppetDeclBuilder::new("child").into()]),
        ..Default::default()
    })
    .await
    .unwrap();

    let child_puppet = test_topology::connect_to_puppet(&realm_proxy, "child").await.unwrap();

    child_puppet.set_health_ok().await.unwrap();

    let accessor = realm_proxy.connect_to_protocol::<ArchiveAccessorMarker>().await.unwrap();
    let data = ArchiveReader::new()
        .with_archive(accessor)
        .add_selector("child:root")
        .with_minimum_schema_count(1)
        .snapshot::<Inspect>()
        .await
        .expect("got inspect data");

    assert_data_tree!(data[0].payload.as_ref().unwrap(), root: {
        "fuchsia.inspect.Health": {
            status: "OK",
            start_timestamp_nanos: AnyProperty,
        }
    });
}

#[fuchsia::test]
async fn read_component_with_hanging_lazy_node() -> Result<(), Error> {
    let realm_proxy = test_topology::create_realm(&ftest::RealmOptions {
        realm_name: Some("hanging_lazy".to_string()),
        puppets: Some(vec![test_topology::PuppetDeclBuilder::new("hanging_data").into()]),
        ..Default::default()
    })
    .await
    .expect("create realm");

    let puppet = test_topology::connect_to_puppet(&realm_proxy, "hanging_data").await?;

    puppet.record_string("child", "value")?;

    let lazy = puppet.record_lazy_values("lazy-node-always-hangs").await?.into_proxy()?;
    lazy.commit(&ftest::CommitOptions { hang: Some(true), ..Default::default() })?;

    puppet.record_int("int", 3)?;

    let accessor = realm_proxy.connect_to_protocol::<ArchiveAccessorMarker>().await?;
    let data = ArchiveReader::new()
        .with_archive(accessor)
        .with_batch_retrieval_timeout_seconds(10)
        .add_selector("hanging_data:*")
        .snapshot::<Inspect>()
        .await
        .expect("got inspect data");

    assert_json_diff!(data[0].payload.as_ref().unwrap(), root: {
        child: "value",
        int: 3i64,
    });

    Ok(())
}

#[fuchsia::test]
async fn read_components_single_selector() -> Result<(), Error> {
    let realm_proxy = test_topology::create_realm(&ftest::RealmOptions {
        puppets: Some(vec![
            test_topology::PuppetDeclBuilder::new("child_a").into(),
            test_topology::PuppetDeclBuilder::new("child_b").into(),
        ]),
        ..Default::default()
    })
    .await
    .expect("create realm");

    let child_a = test_topology::connect_to_puppet(&realm_proxy, "child_a").await?;
    let child_b = test_topology::connect_to_puppet(&realm_proxy, "child_b").await?;

    child_a.set_health_ok().await?;
    child_b.set_health_ok().await?;

    let accessor = realm_proxy.connect_to_protocol::<ArchiveAccessorMarker>().await?;
    let data = ArchiveReader::new()
        .with_archive(accessor)
        .add_selector("child_a:root")
        .snapshot::<Inspect>()
        .await
        .expect("got inspect data");

    // Only inspect from child_a should be reported.
    assert_eq!(data.len(), 1);
    assert_data_tree!(data[0].payload.as_ref().unwrap(), root: {
        "fuchsia.inspect.Health": {
            status: "OK",
            start_timestamp_nanos: AnyProperty,
        }
    });
    assert_eq!(data[0].moniker, "child_a");

    Ok(())
}

#[fuchsia::test]
async fn unified_reader() -> Result<(), Error> {
    let realm_proxy = test_topology::create_realm(&ftest::RealmOptions {
        puppets: Some(vec![test_topology::PuppetDeclBuilder::new("puppet").into()]),
        ..Default::default()
    })
    .await
    .expect("create realm");

    let puppet = test_topology::connect_to_puppet(&realm_proxy, "puppet").await.unwrap();
    puppet.emit_example_inspect_data().unwrap();

    // First, retrieve all of the information in our realm to make sure that everything
    // we expect is present.
    let accessor = realm_proxy.connect_to_protocol::<ArchiveAccessorMarker>().await.unwrap();
    // The following hierarchies are expected:
    //  - puppet1: 1 hierarchy published with InspectSink
    //  - archivist: archivist own hierarchy
    const ALL_INSPECT_ENTRIES: usize = 2;
    retrieve_and_validate_results(accessor, Vec::new(), &UNIFIED_ALL_GOLDEN, ALL_INSPECT_ENTRIES)
        .await;

    // Then verify that from the expected data, we can retrieve one specific value.
    let accessor = realm_proxy.connect_to_protocol::<ArchiveAccessorMarker>().await.unwrap();
    retrieve_and_validate_results(
        accessor,
        vec!["puppet*:*:lazy-*"],
        &UNIFIED_SINGLE_VALUE_GOLDEN,
        // only one puppet exposes lazy nodes.
        1,
    )
    .await;

    // Then verify that subtree selection retrieves all trees under and including root.
    let accessor = realm_proxy.connect_to_protocol::<ArchiveAccessorMarker>().await.unwrap();
    retrieve_and_validate_results(
        accessor,
        vec!["puppet*:root"],
        &UNIFIED_ALL_GOLDEN,
        // we are selecting puppets, so we don't expect archivist own Inspect
        ALL_INSPECT_ENTRIES - 1,
    )
    .await;

    // Then verify that a selector with a correct moniker, but no resolved nodes
    // produces an error schema.
    let accessor = realm_proxy.connect_to_protocol::<ArchiveAccessorMarker>().await.unwrap();
    retrieve_and_validate_results(
        accessor,
        vec!["puppet*:root/non-existent-node:bloop"],
        &UNIFIED_FULL_FILTER_GOLDEN,
        // we are selecting puppet, so we don't expect archivist own inspect
        ALL_INSPECT_ENTRIES - 1,
    )
    .await;

    Ok(())
}

#[fuchsia::test]
async fn feedback_canonical_reader_test() -> Result<(), Error> {
    let realm_proxy = test_topology::create_realm(&ftest::RealmOptions {
        puppets: Some(vec![test_topology::PuppetDeclBuilder::new("test_component").into()]),
        archivist_config: Some(ftest::ArchivistConfig {
            pipelines_path: Some("/pkg/data/config/pipelines/feedback_filtered".to_string()),
            ..Default::default()
        }),
        ..Default::default()
    })
    .await
    .expect("create base topology");

    let puppet = test_topology::connect_to_puppet(&realm_proxy, "test_component").await.unwrap();
    puppet.emit_example_inspect_data().unwrap();

    // First, retrieve all of the information in our realm to make sure that everything
    // we expect is present.
    let accessor = connect_to_feedback_accessor(&realm_proxy).await;
    retrieve_and_validate_results(accessor, Vec::new(), &PIPELINE_ALL_GOLDEN, 1).await;

    // Then verify that from the expected data, we can retrieve one specific value.
    let accessor = connect_to_feedback_accessor(&realm_proxy).await;
    retrieve_and_validate_results(
        accessor,
        vec!["test_component:*:lazy-*"],
        &PIPELINE_SINGLE_VALUE_GOLDEN,
        1,
    )
    .await;

    // Then verify that subtree selection retrieves all trees under and including root.
    let accessor = connect_to_feedback_accessor(&realm_proxy).await;
    retrieve_and_validate_results(accessor, vec!["test_component:root"], &PIPELINE_ALL_GOLDEN, 1)
        .await;

    // Then verify that client selectors dont override the static selectors provided
    // to the archivist.
    let accessor = connect_to_feedback_accessor(&realm_proxy).await;
    retrieve_and_validate_results(
        accessor,
        vec![r"test_component:root:array\:0x15"],
        &PIPELINE_NONOVERLAPPING_SELECTORS_GOLDEN,
        1,
    )
    .await;

    assert!(pipeline_is_filtered(realm_proxy, 1, constants::FEEDBACK_ARCHIVE_ACCESSOR_NAME).await);

    Ok(())
}

#[fuchsia::test]
async fn feedback_disabled_pipeline() -> Result<(), Error> {
    let realm_proxy = test_topology::create_realm(&ftest::RealmOptions {
        puppets: Some(vec![test_topology::PuppetDeclBuilder::new("test_component").into()]),
        archivist_config: Some(ftest::ArchivistConfig {
            pipelines_path: Some(
                "/pkg/data/config/pipelines/feedback_filtering_disabled".to_string(),
            ),
            ..Default::default()
        }),
        ..Default::default()
    })
    .await
    .expect("create base topology");

    let puppet = test_topology::connect_to_puppet(&realm_proxy, "test_component").await.unwrap();
    puppet.emit_example_inspect_data().unwrap();

    assert!(!pipeline_is_filtered(realm_proxy, 2, constants::FEEDBACK_ARCHIVE_ACCESSOR_NAME).await);

    Ok(())
}

#[fuchsia::test]
async fn feedback_pipeline_missing_selectors() -> Result<(), Error> {
    let realm_proxy = test_topology::create_realm(&ftest::RealmOptions {
        puppets: Some(vec![test_topology::PuppetDeclBuilder::new("test_component").into()]),
        archivist_config: Some(ftest::ArchivistConfig::default()),
        ..Default::default()
    })
    .await
    .expect("create base topology");

    let puppet = test_topology::connect_to_puppet(&realm_proxy, "test_component").await.unwrap();
    puppet.emit_example_inspect_data().unwrap();

    assert!(!pipeline_is_filtered(realm_proxy, 2, constants::FEEDBACK_ARCHIVE_ACCESSOR_NAME).await);

    Ok(())
}

#[fuchsia::test]
async fn lowpan_canonical_reader_test() -> Result<(), Error> {
    let realm_proxy = test_topology::create_realm(&ftest::RealmOptions {
        puppets: Some(vec![test_topology::PuppetDeclBuilder::new("test_component").into()]),
        archivist_config: Some(ftest::ArchivistConfig {
            pipelines_path: Some("/pkg/data/config/pipelines/lowpan_filtered".to_string()),
            ..Default::default()
        }),
        ..Default::default()
    })
    .await
    .expect("create base topology");

    let puppet = test_topology::connect_to_puppet(&realm_proxy, "test_component").await.unwrap();
    puppet.emit_example_inspect_data().unwrap();

    // First, retrieve all of the information in our realm to make sure that everything
    // we expect is present.
    let accessor = connect_to_lowpan_accessor(&realm_proxy).await;
    retrieve_and_validate_results(accessor, Vec::new(), &PIPELINE_ALL_GOLDEN, 1).await;

    // Then verify that from the expected data, we can retrieve one specific value.
    let accessor = connect_to_lowpan_accessor(&realm_proxy).await;
    retrieve_and_validate_results(
        accessor,
        vec!["test_component:*:lazy-*"],
        &PIPELINE_SINGLE_VALUE_GOLDEN,
        1,
    )
    .await;

    // Then verify that subtree selection retrieves all trees under and including root.
    let accessor = connect_to_lowpan_accessor(&realm_proxy).await;
    retrieve_and_validate_results(accessor, vec!["test_component:root"], &PIPELINE_ALL_GOLDEN, 1)
        .await;

    // Then verify that client selectors dont override the static selectors provided
    // to the archivist.
    let accessor = connect_to_lowpan_accessor(&realm_proxy).await;
    retrieve_and_validate_results(
        accessor,
        vec![r"test_component:root:array\:0x15"],
        &PIPELINE_NONOVERLAPPING_SELECTORS_GOLDEN,
        1,
    )
    .await;

    assert!(pipeline_is_filtered(realm_proxy, 1, constants::LOWPAN_ARCHIVE_ACCESSOR_NAME).await);

    Ok(())
}

async fn connect_to_feedback_accessor(realm_proxy: &RealmProxyClient) -> ArchiveAccessorProxy {
    realm_proxy
        .connect_to_named_protocol::<ArchiveAccessorMarker>(
            "fuchsia.diagnostics.FeedbackArchiveAccessor",
        )
        .await
        .unwrap()
}

async fn connect_to_lowpan_accessor(realm_proxy: &RealmProxyClient) -> ArchiveAccessorProxy {
    realm_proxy
        .connect_to_named_protocol::<ArchiveAccessorMarker>(
            "fuchsia.diagnostics.LoWPANArchiveAccessor",
        )
        .await
        .unwrap()
}

// Loop indefinitely snapshotting the archive until we get the expected number of
// hierarchies, and then validate that the ordered json represetionation of these hierarchies
// matches the golden file.
//
// If the expected number of hierarchies is 0, set a 10-second timeout to ensure nothing is
// received.
async fn retrieve_and_validate_results(
    accessor: ArchiveAccessorProxy,
    custom_selectors: Vec<&str>,
    golden: &str,
    expected_results_count: usize,
) {
    let mut reader = ArchiveReader::new();
    reader.with_archive(accessor).add_selectors(custom_selectors.into_iter());
    if expected_results_count == 0 {
        reader.with_timeout(fuchsia_zircon::Duration::from_seconds(10));
    } else {
        reader.with_minimum_schema_count(expected_results_count);
    }
    let results = reader.snapshot_raw::<Inspect, serde_json::Value>().await.expect("got result");

    // Convert the json struct into a "pretty" string rather than converting the
    // golden file into a json struct because deserializing the golden file into a
    // struct causes serde_json to convert the u64s into exponential form which
    // causes loss of precision.
    let mut observed_string =
        serde_json::to_string_pretty(&process_results_for_comparison(results))
            .expect("should be able to format the the results as valid json.");
    let mut expected_string = golden.to_string();
    // Remove whitespace from both strings because text editors will do things like
    // requiring json files end in a newline, while the result string is unbounded by
    // newlines. Also, we don't want this test to fail if the only change is to json
    // format within the reader.
    observed_string.retain(|c| !c.is_whitespace());
    expected_string.retain(|c| !c.is_whitespace());
    assert_diff!(&expected_string, &observed_string, "\n", 0);
}

fn process_results_for_comparison(results: serde_json::Value) -> serde_json::Value {
    let mut string_result_array = results
        .as_array()
        .expect("result json is an array of objs.")
        .iter()
        .filter_map(|val| {
            let mut val = val.clone();
            // Filter out the results coming from the archivist, and zero out timestamps
            // that we cant golden test.
            val.as_object_mut().and_then(|obj: &mut serde_json::Map<String, serde_json::Value>| {
                match obj.get(MONIKER_KEY) {
                    Some(serde_json::Value::String(moniker_str)) => {
                        if moniker_str != TEST_ARCHIVIST_MONIKER {
                            let metadata_obj =
                                obj.get_mut(METADATA_KEY).unwrap().as_object_mut().unwrap();
                            metadata_obj.insert(TIMESTAMP_KEY.to_string(), serde_json::json!(0));
                            Some(
                                serde_json::to_string(&serde_json::to_value(obj).unwrap())
                                    .expect("All entries in the array are valid."),
                            )
                        } else {
                            None
                        }
                    }
                    _ => None,
                }
            })
        })
        .collect::<Vec<String>>();

    string_result_array.sort();
    let sorted_results_json_string = format!("[{}]", string_result_array.join(","));
    serde_json::from_str(&sorted_results_json_string).unwrap()
}

async fn pipeline_is_filtered(
    realm: RealmProxyClient,
    expected_results_count: usize,
    accessor_name: &str,
) -> bool {
    let archive_accessor =
        realm.connect_to_named_protocol::<ArchiveAccessorMarker>(accessor_name).await.unwrap();

    let pipeline_results = ArchiveReader::new()
        .with_archive(archive_accessor)
        .with_minimum_schema_count(expected_results_count)
        .snapshot_raw::<Inspect, serde_json::Value>()
        .await
        .expect("got result");

    let all_archive_accessor = realm.connect_to_protocol::<ArchiveAccessorMarker>().await.unwrap();

    let all_results = ArchiveReader::new()
        .with_archive(all_archive_accessor)
        .with_minimum_schema_count(expected_results_count)
        .snapshot_raw::<Inspect, serde_json::Value>()
        .await
        .expect("got result");

    process_results_for_comparison(pipeline_results) != process_results_for_comparison(all_results)
}
