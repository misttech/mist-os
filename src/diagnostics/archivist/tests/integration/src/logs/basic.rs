// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{logs::utils::Listener, test_topology};
use diagnostics_reader::{ArchiveReader, Logs};
use fidl_fuchsia_archivist_test as ftest;
use fidl_fuchsia_diagnostics::ArchiveAccessorMarker;
use fidl_fuchsia_logger::{LogFilterOptions, LogLevelFilter, LogMarker, LogMessage, LogProxy};
use fuchsia_async as fasync;
use fuchsia_component::client;
use fuchsia_syslog_listener as syslog_listener;
use fuchsia_zircon as zx;
use futures::{channel::mpsc, Stream, StreamExt};
use tracing::{info, warn};

fn run_listener(tag: &str, proxy: LogProxy) -> impl Stream<Item = LogMessage> {
    let options = LogFilterOptions {
        filter_by_pid: false,
        pid: 0,
        min_severity: LogLevelFilter::None,
        verbosity: 0,
        filter_by_tid: false,
        tid: 0,
        tags: vec![tag.to_string()],
    };
    let (send_logs, recv_logs) = mpsc::unbounded();
    let l = Listener { send_logs };
    fasync::Task::spawn(async move {
        let fut =
            syslog_listener::run_log_listener_with_proxy(&proxy, l, Some(&options), false, None);
        if let Err(e) = fut.await {
            panic!("test fail {e:?}");
        }
    })
    .detach();
    recv_logs
}

#[fuchsia::test(logging = false)]
async fn listen_for_syslog() {
    let random = rand::random::<u16>();
    let tag = "logger_integration_rust".to_string() + &random.to_string();
    diagnostics_log::initialize(diagnostics_log::PublishOptions::default().tags(&[&tag])).unwrap();
    let log_proxy = client::connect_to_protocol::<LogMarker>().unwrap();
    let incoming = run_listener(&tag, log_proxy);
    info!("my msg: {}", 10);
    warn!("log crate: {}", 20);

    let mut logs: Vec<LogMessage> = incoming.take(2).collect().await;

    // sort logs to account for out-of-order arrival
    logs.sort_by(|a, b| a.time.cmp(&b.time));
    assert_eq!(2, logs.len());
    assert_eq!(logs[1].tags.len(), 1);
    assert_eq!(logs[0].tags[0], tag);
    assert_eq!(logs[0].severity, LogLevelFilter::Info as i32);
    assert_eq!(logs[0].msg, "my msg: 10");
    assert_eq!(logs[1].tags.len(), 1);
    assert_eq!(logs[1].tags[0], tag);
    assert_eq!(logs[1].severity, LogLevelFilter::Warn as i32);
    assert_eq!(logs[1].msg, "log crate: 20");
}

#[fuchsia::test]
async fn listen_for_klog() {
    let realm_proxy = test_topology::create_realm(ftest::RealmOptions {
        archivist_config: Some(ftest::ArchivistConfig::WithKernelLog),
        ..Default::default()
    })
    .await
    .unwrap();

    let log_proxy = realm_proxy.connect_to_protocol::<LogMarker>().await.unwrap();

    let logs = run_listener("klog", log_proxy);
    let msg = format!("logger_integration_rust test_klog {}", rand::random::<u64>());

    let resource = zx::Resource::from(zx::Handle::invalid());
    let debuglog = zx::DebugLog::create(&resource, zx::DebugLogOpts::empty()).unwrap();
    debuglog.write(msg.as_bytes()).unwrap();

    logs.filter(|m| futures::future::ready(m.msg == msg)).next().await;
}

#[fuchsia::test]
async fn listen_for_syslog_routed_stdio() {
    let realm_proxy = test_topology::create_realm(ftest::RealmOptions {
        puppets: Some(vec![ftest::PuppetDecl { name: "stdio-puppet".to_string() }]),
        ..Default::default()
    })
    .await
    .unwrap();

    let accessor = realm_proxy
        .connect_to_protocol::<ArchiveAccessorMarker>()
        .await
        .expect("ArchiveAccessor unavailable");

    let mut reader = ArchiveReader::new();
    reader.with_archive(accessor);
    let (mut logs, mut errors) = reader.snapshot_then_subscribe::<Logs>().unwrap().split_streams();
    let _errors = fasync::Task::spawn(async move {
        while let Some(e) = errors.next().await {
            panic!("error in subscription: {e}");
        }
    });

    let puppet = test_topology::connect_to_puppet(&realm_proxy, "stdio-puppet").await.unwrap();

    let msg = format!("logger_integration_rust test_klog stdout {}", rand::random::<u64>());
    puppet.println(&msg).unwrap();
    logs.by_ref().filter(|m| futures::future::ready(m.msg().unwrap() == msg)).next().await;

    let msg = format!("logger_integration_rust test_klog stderr {}", rand::random::<u64>());
    puppet.eprintln(&msg).unwrap();
    logs.filter(|m| futures::future::ready(m.msg().unwrap() == msg)).next().await;

    // TODO(fxbug.dev/49357): add test for multiline log once behavior is defined.
}
