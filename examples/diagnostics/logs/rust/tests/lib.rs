// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use diagnostics_assertions::assert_data_tree;
use diagnostics_reader::{ArchiveReader, Data, Logs, Severity};
use fidl_fuchsia_component::{BinderMarker, BinderProxy};
use fidl_fuchsia_logger::{LogFilterOptions, LogLevelFilter, LogMarker, LogMessage};
use fuchsia_async::Task;
use fuchsia_component::client::connect_to_protocol;
use fuchsia_syslog_listener::run_log_listener_with_proxy;
use futures::channel::mpsc;
use futures::prelude::*;
use std::pin::pin;

#[fuchsia::test]
async fn launch_example_and_read_hello_world() {
    let url = "#meta/rust_logs_example.cm";
    let _: BinderProxy = connect_to_protocol::<BinderMarker>().expect("launched log example");

    let (logs, mut new_logs, _tasks) = listen_to_logs();

    let mut logs = pin!(logs);

    let (next, new_next) = (logs.next().await.unwrap(), new_logs.next().await.unwrap());
    assert_eq!(Severity::from(next.severity), Severity::Info);
    assert_eq!(next.tags, vec!["logs_example"]);
    assert_eq!(next.msg, "should print");
    assert_ne!(next.pid, 0);
    assert_ne!(next.tid, 0);

    assert_eq!(new_next.metadata.severity, Severity::Info);
    assert_eq!(new_next.metadata.component_url, Some(url.into()));
    assert_eq!(new_next.moniker.to_string(), "logs_example");
    assert_data_tree!(new_next.payload.unwrap(), root:
    {
        "message": {
            "value": "should print",
        }
    });

    let (next, new_next) = (logs.next().await.unwrap(), new_logs.next().await.unwrap());
    assert_eq!(Severity::from(next.severity), Severity::Info);
    assert_eq!(next.tags, vec!["logs_example"]);
    assert_eq!(next.msg, "hello, world! bar=baz foo=1");
    assert_ne!(next.pid, 0);
    assert_ne!(next.tid, 0);

    assert_eq!(new_next.metadata.severity, Severity::Info);
    assert_eq!(new_next.metadata.component_url, Some(url.into()));
    assert_eq!(new_next.moniker.to_string(), "logs_example");
    assert_data_tree!(new_next.payload.unwrap(), root:
    {
        "message":
        {
            "value": "hello, world!",
        },
        "keys":
        {
            "foo": 1i64,
            "bar": "baz",
        }
    });
}

fn listen_to_logs(
) -> (impl Stream<Item = LogMessage>, impl Stream<Item = Data<Logs>>, (Task<()>, Task<()>)) {
    let reader = ArchiveReader::logs();

    let log_proxy = connect_to_protocol::<LogMarker>().unwrap();
    let options = LogFilterOptions {
        filter_by_pid: false,
        pid: 0,
        min_severity: LogLevelFilter::None,
        verbosity: 0,
        filter_by_tid: false,
        tid: 0,
        tags: vec![],
    };
    let (send_logs, recv_logs) = mpsc::unbounded();
    let _old_listener = Task::spawn(async move {
        run_log_listener_with_proxy(&log_proxy, send_logs, Some(&options)).await.unwrap();
    });

    let logs = recv_logs.filter(|m| {
        let from_archivist = m.tags.iter().any(|t| t == "archivist");
        async move { !from_archivist }
    });
    let (new_logs, mut errors) = reader.snapshot_then_subscribe().unwrap().split_streams();

    let _check_errors = Task::spawn(async move {
        if let Some(error) = errors.next().await {
            panic!("log testing client encountered an error: {}", error);
        }
    });

    (logs, new_logs, (_old_listener, _check_errors))
}
