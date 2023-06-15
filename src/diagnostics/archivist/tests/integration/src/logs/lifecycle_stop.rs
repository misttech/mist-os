// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![warn(clippy::all)]

use crate::{constants, test_topology, utils};
use component_events::events::*;
use component_events::matcher::*;
use diagnostics_data::Severity;
use diagnostics_reader::{ArchiveReader, Data, Logs};
use fidl_fuchsia_component as fcomponent;
use fidl_fuchsia_diagnostics as fdiagnostics;
use fidl_fuchsia_logger::{LogFilterOptions, LogLevelFilter, LogMarker, LogMessage};
use fuchsia_async as fasync;
use fuchsia_component_test::RealmInstance;
use fuchsia_component_test::ScopedInstanceFactory;
use fuchsia_syslog_listener::{run_log_listener_with_proxy, LogProcessor};
use futures::{channel::mpsc, StreamExt};

const LOGGING_COMPONENT: &str = "logging_component";

#[fuchsia::test]
async fn embedding_stop_api_for_log_listener() {
    let instance = initialize_topology().await;

    let options = LogFilterOptions {
        filter_by_pid: false,
        pid: 0,
        min_severity: LogLevelFilter::None,
        verbosity: 0,
        filter_by_tid: false,
        tid: 0,
        tags: vec!["logging component".to_owned()],
    };
    let (send_logs, recv_logs) = mpsc::unbounded();

    let log_proxy = instance.root.connect_to_protocol_at_exposed_dir::<LogMarker>().unwrap();
    fasync::Task::spawn(async move {
        let l = Listener { send_logs };
        run_log_listener_with_proxy(&log_proxy, l, Some(&options), false, None).await.unwrap();
    })
    .detach();

    let mut event_stream = EventStream::open().await.unwrap();

    run_logging_component(&instance, &mut event_stream).await;

    // this will trigger Lifecycle.Stop.
    drop(instance);

    // collect all logs
    let logs = recv_logs.map(|l| (l.severity as i8, l.msg)).collect::<Vec<_>>().await;

    assert_eq!(
        logs,
        vec![
            (fdiagnostics::Severity::Debug.into_primitive() as i8, "my debug message.".to_owned()),
            (fdiagnostics::Severity::Info.into_primitive() as i8, "my info message.".to_owned()),
            (fdiagnostics::Severity::Warn.into_primitive() as i8, "my warn message.".to_owned()),
        ]
    );
}

#[fuchsia::test]
async fn embedding_stop_api_works_for_batch_iterator() {
    let instance = initialize_topology().await;
    let accessor = instance
        .root
        .connect_to_protocol_at_exposed_dir::<fdiagnostics::ArchiveAccessorMarker>()
        .expect("cannot connect to accessor proxy");
    let subscription =
        ArchiveReader::new().with_archive(accessor).snapshot_then_subscribe().expect("subscribed");

    let mut event_stream = EventStream::open().await.unwrap();

    run_logging_component(&instance, &mut event_stream).await;

    // this will trigger Lifecycle.Stop.
    drop(instance);

    // collect all logs
    let logs = subscription
        .map(|result| {
            let data: Data<Logs> = result.expect("got result");
            (data.metadata.severity, data.msg().unwrap().to_owned())
        })
        .collect::<Vec<_>>()
        .await;

    assert_eq!(
        logs,
        vec![
            (Severity::Debug, "my debug message.".to_owned()),
            (Severity::Info, "my info message.".to_owned()),
            (Severity::Warn, "my warn message.".to_owned()),
        ]
    );
}

async fn initialize_topology() -> RealmInstance {
    let (builder, test_realm) =
        test_topology::create(test_topology::Options::default()).await.unwrap();
    test_topology::add_collection(&test_realm, "coll").await.unwrap();
    test_topology::expose_test_realm_protocol(&builder, &test_realm).await;
    builder.build().await.expect("create instance")
}

async fn run_logging_component(realm: &RealmInstance, event_stream: &mut EventStream) {
    let realm_proxy =
        realm.root.connect_to_protocol_at_exposed_dir::<fcomponent::RealmMarker>().unwrap();
    let mut instance = ScopedInstanceFactory::new("coll")
        .with_realm_proxy(realm_proxy)
        .new_named_instance(LOGGING_COMPONENT, constants::LOGGING_COMPONENT_URL)
        .await
        .unwrap();

    // launch our child, wait for it to exit, and destroy (so all its outgoing log connections
    // are processed) before asserting on its logs
    let _ = instance.connect_to_protocol_at_exposed_dir::<fcomponent::BinderMarker>().unwrap();
    utils::wait_for_component_stopped_event(
        realm.root.child_name(),
        &format!("coll:{LOGGING_COMPONENT}"),
        ExitStatusMatcher::Clean,
        event_stream,
    )
    .await;
    let waiter = instance.take_destroy_waiter();
    drop(instance);
    waiter.await.unwrap();
}

struct Listener {
    send_logs: mpsc::UnboundedSender<LogMessage>,
}

impl LogProcessor for Listener {
    fn log(&mut self, message: LogMessage) {
        self.send_logs.unbounded_send(message).unwrap();
    }

    fn done(&mut self) {
        panic!("this should not be called");
    }
}
