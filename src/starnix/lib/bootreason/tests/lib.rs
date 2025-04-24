// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use component_events::events::{EventStream, ExitStatus, Stopped};
use component_events::matcher::EventMatcher;
use fidl_fuchsia_feedback::{
    LastReboot, LastRebootInfoProviderMarker, LastRebootInfoProviderRequest,
    LastRebootInfoProviderRequestStream, RebootReason,
};
use fuchsia_async as fasync;
use fuchsia_component::server as fserver;
use fuchsia_component_test::{
    Capability, ChildOptions, ChildRef, LocalComponentHandles, RealmBuilder, RealmBuilderParams,
    RealmInstance, Route,
};
use futures::{StreamExt, TryStreamExt};
use log::info;

/// Builds a test realm where the LastReboot info is `reason`.
async fn build_test_realm(reason: &LastReboot) -> RealmInstance {
    info!("Building realm last_reboot={reason:?}");
    let builder = RealmBuilder::with_params(
        RealmBuilderParams::new().from_relative_url("#meta/test_realm.cm"),
    )
    .await
    .unwrap();

    let stub_last_reboot_info = StubLastRebootInfo::new(reason);
    let fake_last_reboot_info_ref = builder
        .add_local_child(
            "fake_last_reboot_info",
            move |handles| Box::pin(stub_last_reboot_info.clone().serve(handles)),
            ChildOptions::new(),
        )
        .await
        .unwrap();

    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol::<LastRebootInfoProviderMarker>())
                .from(&fake_last_reboot_info_ref)
                .to(&ChildRef::from("kernel")),
        )
        .await
        .unwrap();

    builder.build().await.unwrap()
}

/// Reads an arbitrary file within the starnix realm.
async fn read_starnix_file(filename: &str, realm: &RealmInstance) -> String {
    let read_flags = fuchsia_fs::PERM_READABLE;
    let file = fuchsia_fs::directory::open_file(realm.root.get_exposed_dir(), filename, read_flags)
        .await
        .unwrap();

    let contents = fuchsia_fs::file::read(&file).await.unwrap();
    String::from_utf8(contents).unwrap()
}

#[fasync::run_singlethreaded(test)]
async fn test_no_errors_reboot_normal() {
    let reboot = LastReboot {
        graceful: Some(true),
        uptime: Some(65000),
        reason: Some(RebootReason::UserRequest),
        ..Default::default()
    };
    let realm_instance = build_test_realm(&reboot).await;
    let cmdline = read_starnix_file("/fs_root/proc/cmdline", &realm_instance).await;
    assert!(
        cmdline.contains("android.bootreason=reboot,userrequested"),
        "cmdline ({cmdline}) has wrong bootreason"
    );
}

#[fasync::run_singlethreaded(test)]
async fn test_kernel_panic() {
    let reboot = LastReboot {
        graceful: Some(false),
        uptime: Some(65000),
        reason: Some(RebootReason::KernelPanic),
        ..Default::default()
    };
    let realm_instance = build_test_realm(&reboot).await;
    let cmdline = read_starnix_file("/fs_root/proc/cmdline", &realm_instance).await;

    assert!(
        cmdline.contains("android.bootreason=kernel_panic"),
        "cmdline ({cmdline}) has wrong bootreason"
    );
}

#[fasync::run_singlethreaded(test)]
async fn test_pstore_present() {
    let reboot = LastReboot {
        graceful: Some(true),
        uptime: Some(65000),
        reason: Some(RebootReason::UserRequest),
        ..Default::default()
    };
    let mut events = EventStream::open().await.unwrap();
    let realm_instance = build_test_realm(&reboot).await;
    let realm_moniker = format!("realm_builder:{}", realm_instance.root.child_name());
    let mount_pstore_moniker = format!("{realm_moniker}/mount_pstore");

    // Wait for /sys and /sys/fs/pstore to be mounted inside the starnix realm.
    info!(mount_pstore_moniker:%; "waiting for mount_pstore to exit");
    let stopped = EventMatcher::ok()
        .moniker(&mount_pstore_moniker)
        .wait::<Stopped>(&mut events)
        .await
        .unwrap();
    let status = stopped.result().unwrap().status;
    info!(status:?; "mount_pstore stopped");
    assert_eq!(status, ExitStatus::Clean);

    let console =
        read_starnix_file("/fs_root/sys/fs/pstore/console-ramoops", &realm_instance).await;
    assert!(
        console.contains("Fuchsia Console Ramoops\n"),
        "console-ramoops ({console}) has unexpected contents"
    );
}

#[derive(Clone)]
struct StubLastRebootInfo {
    last_reboot: LastReboot,
}

impl StubLastRebootInfo {
    fn new(last_reboot: &LastReboot) -> Self {
        Self { last_reboot: last_reboot.clone() }
    }

    async fn serve(self, handles: LocalComponentHandles) -> Result<(), anyhow::Error> {
        let mut fs = fserver::ServiceFs::new();
        fs.dir("svc").add_fidl_service(|client: LastRebootInfoProviderRequestStream| client);
        fs.serve_connection(handles.outgoing_dir)?;

        fs.for_each_concurrent(0, |stream: LastRebootInfoProviderRequestStream| async {
            stream
                .try_for_each(|request| {
                    let last_reboot = self.last_reboot.clone();
                    async move {
                        match request {
                            LastRebootInfoProviderRequest::Get { responder } => {
                                responder.send(&last_reboot).expect("failed to send LastReboot");
                            }
                        }
                        Ok(())
                    }
                })
                .await
                .expect("failed to serve LastReboot request stream");
        })
        .await;

        Ok(())
    }
}
