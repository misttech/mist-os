// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use diagnostics_data::{Data, LogsField};
use diagnostics_reader::{ArchiveReader, DiagnosticsHierarchy, Logs, SubscriptionResultsStream};
use fidl_fuchsia_diagnostics::Severity;
use fuchsia_async::{Task, Timer};
use fuchsia_zircon::AsHandleRef as _;
use futures::prelude::*;
use std::{pin::pin, time::Duration};
use test_case::test_case;
use tracing::trace;

type MakeTaskFn = fn(std::pin::Pin<Box<dyn Future<Output = ()> + Send + 'static>>) -> Task<()>;

// Provide a wrapper with a literal invocation of `Task::spawn`, because passing
// `Task::spawn` into the test case interferes with `track_caller`.
const TASK_SPAWN_LINE_NUM: u32 = line!() + 2;
fn task_spawn(fut: impl Future<Output = ()> + Send + 'static) -> Task<()> {
    Task::spawn(fut)
}

// Provide a wrapper with a literal invocation of `Task::local`, because passing
// `Task::spawn` into the test case interferes with `track_caller`.
const TASK_LOCAL_LINE_NUM: u32 = line!() + 2;
fn task_local(fut: impl Future<Output = ()> + Send + 'static) -> Task<()> {
    Task::local(fut)
}

#[test_case(task_spawn, TASK_SPAWN_LINE_NUM; "create_with_spawn")]
#[test_case(task_local, TASK_LOCAL_LINE_NUM; "create_with_local")]
#[fuchsia_async::run_singlethreaded(test)]
async fn initialize_logging_and_find_hung_tasks(make_task: MakeTaskFn, make_task_line_num: u32) {
    // initialize logging at TRACE severity without spawning any tasks to the runtime
    diagnostics_log::initialize(
        diagnostics_log::PublishOptions::default()
            .minimum_severity(Severity::Trace)
            .listen_for_interest_updates(false),
    )
    .unwrap();

    // start listening to our own logs
    let mut events = EventsFromLogs::new().await;

    // send a plain test message through
    trace!("init'd");
    events.next().await.expect_message("init'd");

    // create a task with a known source location that will hang until we send to this channel
    let (send, recv) = futures::channel::oneshot::channel();
    let _spawned = make_task(Box::pin(async move {
        trace!("waiting for oneshot channel message");
        recv.await.unwrap();
        trace!("received oneshot channel message");
    }));
    // the above task should be logged as spawned from this file and the above statement
    let expected_source_prefix = format!("{}:{}:", file!(), make_task_line_num);

    // check that we got the task spawn event
    let expected_id;
    match events.next().await {
        LogEvent::TaskSpawned { id, source } => {
            if !source.starts_with(&expected_source_prefix) {
                panic!(
                    "TaskSpawned: expected source starting with {}, got {}",
                    expected_source_prefix, source
                );
            }
            expected_id = id;
        }
        other => panic!("expected task spawn event, got {:?}", other),
    }

    // wait for the task to print its first log message
    events.next().await.expect_message("waiting for oneshot channel message");

    // unblock the task in a sub-scope so we can keep a mutable borrow into `events` for it
    {
        // make sure we haven't gotten any more messages yet by attempting to out-wait latency in
        // the logging pipeline. using a delay here means that legitimate failures will appear as
        // flakes, but given the lack of a "flush everything and wait" api from archivist i think
        // it's the best we can do for now
        let mut pending_until_channel_send = pin!(events.next());
        Timer::new(Duration::from_secs(1)).await;
        assert!(futures::poll!(&mut pending_until_channel_send).is_pending());

        // allow the task to proceed, make sure we get both messages we expect
        send.send(()).unwrap();
        pending_until_channel_send.await.expect_message("received oneshot channel message");
    }

    match events.next().await {
        LogEvent::TaskCompleted { id, source } => {
            if !source.starts_with(&expected_source_prefix) {
                panic!(
                    "TaskCompleted: expected source starting with {}, got {}",
                    expected_source_prefix, source
                );
            }
            assert_eq!(id, expected_id);
        }
        other => panic!("expected task complete event, got {:?}", other),
    }
}

#[derive(Debug)]
enum LogEvent {
    Message(String),
    TaskSpawned { id: u64, source: String },
    TaskCompleted { id: u64, source: String },
}

impl LogEvent {
    #[track_caller]
    fn expect_message(&self, expected: &str) {
        match self {
            LogEvent::Message(msg) => assert_eq!(msg, expected),
            other => panic!("expected string message, got {:?}", other),
        }
    }
}

struct EventsFromLogs {
    logs: SubscriptionResultsStream<Data<Logs>>,
    pid: u64,
}

impl EventsFromLogs {
    async fn new() -> Self {
        let reader = ArchiveReader::new();
        // split_streams is needed here to ensure parallel execution.
        // If this isn't ran in parallel, the ordering required by this
        // test never happens.
        let (logs, _errors) = reader.snapshot_then_subscribe::<Logs>().unwrap().split_streams();
        let mut events = EventsFromLogs {
            logs,
            pid: fuchsia_runtime::process_self().get_koid().unwrap().raw_koid(),
        };

        // creating the log stream from the archivereader spawns a task, which creates a log message
        match events.next().await {
            LogEvent::TaskSpawned { id: _, source } => {
                // this is a hack that will break as soon as the library changes names, alas
                if !source.contains("diagnostics/reader") {
                    panic!("unrecognized source of diagnostics reader task: {}", source);
                }
            }
            other => panic!("unexpected log event: {:?}", other),
        }

        events
    }

    async fn next(&mut self) -> LogEvent {
        let next = loop {
            // skip over logs from other processes. this ensures that logs messages from other test
            // cases do not interfere with the current test case.
            let next = self.logs.next().await.unwrap();
            if next.metadata.pid.unwrap() == self.pid {
                break next;
            }
        };
        let payload = next.payload.unwrap();
        assert_eq!(payload.name, "root");

        match get_log_message(&payload) {
            "Task spawned" => {
                let (id, source) = get_task_id_and_source(&payload);
                LogEvent::TaskSpawned { id, source }
            }
            "Task completed" => {
                let (id, source) = get_task_id_and_source(&payload);
                LogEvent::TaskCompleted { id, source }
            }
            other => LogEvent::Message(other.to_string()),
        }
    }
}

fn get_log_message(payload: &DiagnosticsHierarchy<LogsField>) -> &str {
    payload.get_child("message").unwrap().get_property("value").unwrap().string().unwrap()
}

fn get_task_id_and_source(payload: &DiagnosticsHierarchy<LogsField>) -> (u64, String) {
    let keys = payload.get_child("keys").unwrap();
    let id = keys.get_property("id").unwrap().uint().unwrap();
    let source = keys.get_property("source").unwrap().string().unwrap().to_string();
    (id, source)
}
