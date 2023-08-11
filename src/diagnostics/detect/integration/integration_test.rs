// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

/**
 * This program integration-tests the triage-detect program using the OpaqueTest library
 * to inject a fake CrashReporter, ArchiveAccessor, and config-file directory.
 *
 * triage-detect will be able to fetch Diagnostic data, evaluate it according to the .triage
 * files it finds, and request whatever crash reports are appropriate. Meanwhile, the fakes
 * will be writing TestEvent entries to a stream for later evaluation.
 *
 * Each integration test is stored in a file whose name starts with "test". This supplies:
 * 1) Some number of Diagnostic data strings. When the program tries to fetch Diagnostic data
 *  after these strings are exhausted, the fake ArchiveAccessor writes to a special "done" channel
 *  to terminate the test.
 * 2) Some number of config files to place in the fake directory.
 * 3) A vector of vectors of crash report signatures. The inner vector should match the crash
 *  report requests sent between each fetch of Diagnostic data. Order of the inner vector does
 *  not matter, but duplicates do matter.
 */
use {
    anyhow::Error, fidl::endpoints::create_endpoints, fidl_fuchsia_diagnostics_test as ftest,
    fidl_fuchsia_testing_harness::RealmProxy_Marker,
    fuchsia_component::client::connect_to_protocol, fuchsia_zircon as zx, futures::StreamExt,
    realm_proxy::Error::OperationError, std::cmp::Ordering, test_case::test_case, tracing::*,
};

// Test that the "repeat" field of snapshots works correctly.
mod test_snapshot_throttle;
// Test that the "trigger" field of snapshots works correctly.
mod test_trigger_truth;
// Test that no report is filed unless "config.json" has the magic contents.
mod test_filing_enable;
// Test that all characters other than [a-z-] are converted to that set.
mod test_snapshot_sanitizing;

#[test_case(test_snapshot_throttle::test())]
#[test_case(test_trigger_truth::test())]
#[test_case(test_snapshot_sanitizing::test())]
#[test_case(test_filing_enable::test_with_enable())]
#[test_case(test_filing_enable::test_bad_enable())]
#[test_case(test_filing_enable::test_false_enable())]
#[test_case(test_filing_enable::test_no_enable())]
#[test_case(test_filing_enable::test_without_file())]
#[fuchsia::test]
async fn triage_detect_test(test_data: TestData) -> Result<(), Error> {
    info!("running test case {}", test_data.name);

    let realm_factory = connect_to_protocol::<ftest::RealmFactoryMarker>()?;
    realm_factory.set_realm_options(test_data.realm_options).await?.map_err(OperationError)?;
    // The realm is disposed once _ignore is dropped.
    let (_ignore, realm_server) = create_endpoints::<RealmProxy_Marker>();
    realm_factory.create_realm(realm_server).await?.map_err(OperationError)?;

    let event_proxy = realm_factory.get_triage_detect_events().await?.into_proxy()?;
    let mut actual_events = drain(event_proxy.take_event_stream()).await;
    let mut expected_events = test_data.expected_events;

    actual_events.sort_unstable_by(compare_crash_signatures_only);
    expected_events.sort_unstable_by(compare_crash_signatures_only);
    assert_events_eq(&expected_events, &actual_events);
    Ok(())
}

async fn drain(mut stream: ftest::TriageDetectEventsEventStream) -> Vec<TestEvent> {
    let mut events = vec![];
    while let Some(Ok(event)) = stream.next().await {
        info!("received event: {:?}", event);
        let event = TestEvent::from(event);
        match event {
            TestEvent::OnDone {} => return events,
            TestEvent::OnBail {} => {
                // Record the event so tests can assert whether we bailed early.
                events.push(event);
                return events;
            }
            _ => events.push(event),
        };
    }
    events
}

fn assert_events_eq(expected: &Vec<TestEvent>, actual: &Vec<TestEvent>) {
    if let Some(index) = find_first_different_index(&expected, &actual) {
        assert!(
            false,
            "\n\n\
             Wanted events: {:?}\n\
             Got events:    {:?}\n\
             Which differ at index {}:\n\
             * Want: {:?}\n\
             * Got:  {:?}\n\
            \n\n",
            expected, actual, index, expected[index], actual[index],
        );
    }
}

fn find_first_different_index(left: &Vec<TestEvent>, right: &Vec<TestEvent>) -> Option<usize> {
    match left.iter().zip(right.iter()).position(|(l, r)| l != r) {
        Some(index) => Some(index),
        None => {
            if left.len() == right.len() {
                return None;
            }
            Some(std::cmp::min(left.len(), right.len()) - 1)
        }
    }
}

// A comparator used to sort test events by crash signature.
// Subgroups of crash reports that occur between diagnostics fetches are sorted,
// but fetch events are not sorted. For example: the events
// {C, A, FETCH, D, B, FETCH, G} are sorted as:
// {A, C, FETCH, B, D, FETCH, G}.
fn compare_crash_signatures_only(prev: &TestEvent, next: &TestEvent) -> Ordering {
    if let TestEvent::OnCrashReport { crash_signature, .. } = prev {
        let left = crash_signature;
        if let TestEvent::OnCrashReport { crash_signature, .. } = next {
            let right = crash_signature;
            return left.partial_cmp(right).unwrap();
        }
    }

    Ordering::Equal
}

pub(crate) fn create_vmo(contents: impl Into<String>) -> zx::Vmo {
    let contents = contents.into();
    let vmo = zx::Vmo::create(contents.len() as u64).unwrap();
    vmo.write(contents.as_bytes(), 0).unwrap();
    vmo
}

pub(crate) struct TestData {
    name: String,
    realm_options: ftest::RealmOptions,
    expected_events: Vec<TestEvent>,
}

impl TestData {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            realm_options: ftest::RealmOptions { ..Default::default() },
            expected_events: vec![],
        }
    }

    pub fn add_triage_config(mut self, config_js: impl Into<String>) -> Self {
        let triage_configs = self.realm_options.triage_configs.get_or_insert_with(|| vec![]);
        triage_configs.push(create_vmo(config_js));
        self
    }

    pub fn add_inspect_data(mut self, inspect_data_js: impl Into<String>) -> Self {
        let inspect_data = self.realm_options.inspect_data.get_or_insert_with(|| vec![]);
        inspect_data.push(create_vmo(inspect_data_js));
        self
    }

    pub fn set_program_config(mut self, program_config_js: impl Into<String>) -> Self {
        self.realm_options.program_config.replace(create_vmo(program_config_js));
        self
    }

    pub fn expect_events(mut self, events: Vec<TestEvent>) -> Self {
        self.expected_events = events;
        self
    }
}

// An TriageDetectEventsEvent representation that allows us to compare events.
#[derive(Debug, PartialEq)]
pub(crate) enum TestEvent {
    OnBail,
    OnDiagnosticFetch,
    OnDone,
    OnCrashReport { crash_signature: String, crash_program_name: String },
}

impl From<ftest::TriageDetectEventsEvent> for TestEvent {
    fn from(event: ftest::TriageDetectEventsEvent) -> Self {
        match event {
            ftest::TriageDetectEventsEvent::OnDone {} => TestEvent::OnDone,
            ftest::TriageDetectEventsEvent::OnBail {} => TestEvent::OnBail,
            ftest::TriageDetectEventsEvent::OnDiagnosticFetch {} => TestEvent::OnDiagnosticFetch,
            ftest::TriageDetectEventsEvent::OnCrashReport {
                crash_signature,
                crash_program_name,
            } => TestEvent::OnCrashReport { crash_signature, crash_program_name },
            _ => panic!("unknown event {:?}", event),
        }
    }
}
