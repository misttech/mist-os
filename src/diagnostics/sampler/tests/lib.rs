// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use diagnostics_assertions::{assert_data_tree, AnyProperty};
use diagnostics_reader::{ArchiveReader, Inspect};
use fidl_fuchsia_component::BinderMarker;
use fidl_fuchsia_metrics_test::MetricEventLoggerQuerierMarker;
use fidl_fuchsia_mockrebootcontroller::MockRebootControllerMarker;
use fidl_fuchsia_samplertestcontroller::SamplerTestControllerMarker;
use fuchsia_component::client::{connect_to_protocol_at, connect_to_protocol_at_path};
use realm_client::InstalledNamespace;
use utils::{Event, EventVerifier};
use {fidl_fuchsia_diagnostics as fdiagnostics, fuchsia_async as fasync};

mod test_topology;
mod utils;

async fn wait_for_single_counter_inspect(ns: &InstalledNamespace) {
    let accessor = connect_to_protocol_at::<fdiagnostics::ArchiveAccessorMarker>(&ns).unwrap();
    let _ = ArchiveReader::new()
        .with_archive(accessor)
        .add_selector(format!("{}:root", test_topology::COUNTER_NAME))
        .snapshot::<Inspect>()
        .await
        .expect("got inspect data");
}

/// Runs the Sampler and a test component that can have its inspect properties
/// manipulated by the test via fidl, and uses cobalt mock and log querier to
/// verify that the sampler observers changes as expected, and logs them to
/// cobalt as expected.
#[fuchsia::test]
async fn event_count_sampler_test() {
    let ns = test_topology::create_realm().await.expect("initialized topology");
    let test_app_controller = connect_to_protocol_at::<SamplerTestControllerMarker>(&ns).unwrap();
    wait_for_single_counter_inspect(&ns).await;
    let reboot_controller = connect_to_protocol_at::<MockRebootControllerMarker>(&ns).unwrap();
    let logger_querier = connect_to_protocol_at::<MetricEventLoggerQuerierMarker>(&ns).unwrap();
    let _sampler_binder = connect_to_protocol_at_path::<BinderMarker>(format!(
        "{}/fuchsia.component.SamplerBinder",
        ns.prefix()
    ))
    .unwrap();

    let mut project_5_events = EventVerifier::new(&logger_querier, 5);
    let mut project_13_events = EventVerifier::new(&logger_querier, 13);

    test_app_controller.increment_int(1).await.unwrap();

    project_5_events
        .validate_with_count(
            vec![
                Event { id: 101, value: 1, codes: vec![0, 0] },
                Event { id: 102, value: 10, codes: vec![0, 0] },
                Event { id: 103, value: 20, codes: vec![0, 0] },
            ],
            "initial in event_count",
        )
        .await;

    // Make sure I'm getting the expected FIRE events.
    // Three components in components.json5: integer_1, integer_2, and integer_42
    //   with codes 121, 122, and 142, respectively.
    // Two metrics in fire_1.json5: id 2 (event codes [0, 0]); id 3 (no event codes)
    // One metric in fire_2.json5: id 4 (event codes [14, 15])
    // One metric in fire_3.json5: id 5 (event codes [99])
    //   metrics in fire_1 and fire_2 are by moniker; in fire_3 is by hash
    //   metrics in fire_1 and fire_2 all use the same selector and should pick
    //     up the same published Inspect data.
    // The component instance ID file (in realm_factory/src/mocks.rs) gives an instance ID
    //   for the integer_42 component.
    // The Inspect data (from test_component/src/main.rs) publishes data
    //   for integer_1 (10) and integer_2 (20) by moniker, and integer_42 by ID (42).
    //   Also other data that shouldn't be picked up by FIRE.
    // So I'd expect id's 2, 3, and 4, for integer_1 and integer_2, and id 5 for integer_42.
    //   Seven events total.
    project_13_events
        .validate_with_count(
            vec![
                Event { id: 2, value: 10, codes: vec![121, 0, 0] },
                Event { id: 2, value: 20, codes: vec![122, 0, 0] },
                Event { id: 3, value: 10, codes: vec![121] },
                Event { id: 3, value: 20, codes: vec![122] },
                Event { id: 4, value: 10, codes: vec![121, 14, 15] },
                Event { id: 4, value: 20, codes: vec![122, 14, 15] },
                Event { id: 5, value: 42, codes: vec![142, 99] },
            ],
            "First FIRE events",
        )
        .await;

    // We want to guarantee a sample takes place before we increment the value again.
    // This is to verify that despite two samples taking place, the count type isn't uploaded with no diff
    // and the metric that is upload_once isn't sampled again.
    test_app_controller.wait_for_sample().await.unwrap().unwrap();

    project_5_events
        .validate_with_count(
            vec![Event { id: 102, value: 10, codes: vec![0, 0] }],
            "second in event_count",
        )
        .await;

    test_app_controller.increment_int(1).await.unwrap();

    // Same FIRE events as before, except metric ID 3 is upload_once.
    project_13_events
        .validate_with_count(
            vec![
                Event { id: 2, value: 10, codes: vec![121, 0, 0] },
                Event { id: 2, value: 20, codes: vec![122, 0, 0] },
                Event { id: 4, value: 10, codes: vec![121, 14, 15] },
                Event { id: 4, value: 20, codes: vec![122, 14, 15] },
                Event { id: 5, value: 42, codes: vec![142, 99] },
            ],
            "Second FIRE events",
        )
        .await;

    test_app_controller.wait_for_sample().await.unwrap().unwrap();

    project_5_events
        .validate_with_count(
            vec![
                // Even though we incremented metric-1 its value stays at 1 since it's being cached.
                Event { id: 101, value: 1, codes: vec![0, 0] },
                Event { id: 102, value: 10, codes: vec![0, 0] },
            ],
            "before reboot in event_count",
        )
        .await;

    // FIRE continues...
    project_13_events
        .validate_with_count(
            vec![
                Event { id: 2, value: 10, codes: vec![121, 0, 0] },
                Event { id: 2, value: 20, codes: vec![122, 0, 0] },
                Event { id: 4, value: 10, codes: vec![121, 14, 15] },
                Event { id: 4, value: 20, codes: vec![122, 14, 15] },
                Event { id: 5, value: 42, codes: vec![142, 99] },
            ],
            "More FIRE events",
        )
        .await;

    // trigger_reboot calls the on_reboot callback that drives sampler shutdown. this
    // should await until sampler has finished its cleanup, which means we should have some events
    // present when we're done, and the sampler task should be finished.
    reboot_controller.trigger_reboot().await.unwrap().unwrap();

    project_5_events
        .validate_with_count(
            vec![
                // The metric configured to run every 3000 seconds gets polled, and gets an undiffed
                // report of its values.
                Event { id: 104, value: 2, codes: vec![0, 0] },
                Event { id: 102, value: 10, codes: vec![0, 0] },
            ],
            "after reboot in event_count",
        )
        .await;

    // Just to make sure
    project_13_events
        .validate_with_count(
            vec![
                Event { id: 2, value: 10, codes: vec![121, 0, 0] },
                Event { id: 2, value: 20, codes: vec![122, 0, 0] },
                Event { id: 4, value: 10, codes: vec![121, 14, 15] },
                Event { id: 4, value: 20, codes: vec![122, 14, 15] },
                Event { id: 5, value: 42, codes: vec![142, 99] },
            ],
            "Last FIRE events",
        )
        .await;
}

/// Runs the Sampler and a test component that can have its inspect properties
/// manipulated by the test via fidl, and verifies that Sampler publishes
/// the expected Inspect data reflecting its configuration.
#[fuchsia::test]
async fn reboot_server_crashed_test() {
    let ns = test_topology::create_realm().await.expect("initialized topology");
    let test_app_controller = connect_to_protocol_at::<SamplerTestControllerMarker>(&ns).unwrap();
    wait_for_single_counter_inspect(&ns).await;
    let reboot_controller = connect_to_protocol_at::<MockRebootControllerMarker>(&ns).unwrap();
    let logger_querier = connect_to_protocol_at::<MetricEventLoggerQuerierMarker>(&ns).unwrap();
    let _sampler_binder = connect_to_protocol_at_path::<BinderMarker>(format!(
        "{}/fuchsia.component.SamplerBinder",
        ns.prefix()
    ))
    .unwrap();
    let mut project_5_events = EventVerifier::new(&logger_querier, 5);

    // Crash the reboot server to verify that sampler continues to sample.
    reboot_controller.crash_reboot_channel().await.unwrap().unwrap();

    test_app_controller.increment_int(1).await.unwrap();

    project_5_events
        .validate_with_count(
            vec![
                Event { id: 101, value: 1, codes: vec![0, 0] },
                Event { id: 102, value: 10, codes: vec![0, 0] },
                Event { id: 103, value: 20, codes: vec![0, 0] },
            ],
            "initial in crashed",
        )
        .await;

    // We want to guarantee a sample takes place before we increment the value again.
    // This is to verify that despite two samples taking place, the count type isn't uploaded with
    // no diff and the metric that is upload_once isn't sampled again.
    test_app_controller.wait_for_sample().await.unwrap().unwrap();

    project_5_events
        .validate_with_count(
            vec![Event { id: 102, value: 10, codes: vec![0, 0] }],
            "second in crashed",
        )
        .await;
}

/// Runs the Sampler and a test component that can have its inspect properties
/// manipulated by the test via fidl. Verifies that Sampler publishes its own
/// status correctly in its own Inspect data.
#[fuchsia::test]
async fn sampler_inspect_test() {
    let ns = test_topology::create_realm().await.expect("initialized topology");
    let _sampler_binder = connect_to_protocol_at_path::<BinderMarker>(format!(
        "{}/fuchsia.component.SamplerBinder",
        ns.prefix()
    ))
    .unwrap();

    let hierarchy = loop {
        let accessor = connect_to_protocol_at::<fdiagnostics::ArchiveAccessorMarker>(&ns).unwrap();
        // Observe verification shows up in inspect.
        let mut data = ArchiveReader::new()
            .with_archive(accessor)
            .add_selector(format!("{}:root", test_topology::SAMPLER_NAME))
            .snapshot::<Inspect>()
            .await
            .expect("got inspect data");

        let hierarchy = data.pop().expect("one result").payload.expect("payload is not none");
        if hierarchy.get_child("sampler_executor_stats").is_none() {
            fasync::Timer::new(fasync::MonotonicInstant::after(
                zx::MonotonicDuration::from_millis(100),
            ))
            .await;
            continue;
        }
        break hierarchy;
    };
    assert_data_tree!(
        hierarchy,
        root: {
            config: {
                minimum_sample_rate_sec: 1u64,
                configs_path: "/pkg/data/config",
            },
            sampler_executor_stats: {
                healthily_exited_samplers: 0u64,
                errorfully_exited_samplers: 0u64,
                reboot_exited_samplers: 0u64,
                total_project_samplers_configured: 5u64,
                project_5: {
                    project_sampler_count: 2u64,
                    metrics_configured: 4u64,
                    cobalt_logs_sent: AnyProperty,
                },
                project_13: {
                    project_sampler_count: 3u64,
                    metrics_configured: 10u64,
                    cobalt_logs_sent: AnyProperty,
                },
            },
            "fuchsia.inspect.Health": {
                start_timestamp_nanos: AnyProperty,
                status: AnyProperty
            }
        }
    );
}
