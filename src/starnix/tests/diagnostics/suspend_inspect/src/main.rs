// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// XXX(fmil): remove
#![allow(unused)]

use anyhow::{Context, Result};
use component_events::events::{EventStream, Started, Stopped};
use component_events::matcher::EventMatcher;
use diagnostics_reader::{ArchiveReader, Inspect};
use fasync::{DurationExt, TimeoutExt};
use futures::{TryFutureExt, TryStreamExt};
use tracing::info;
use {
    fidl_fuchsia_test_syscalls as ffts, fidl_fuchsia_testing_harness as ffth,
    fuchsia_async as fasync, fuchsia_component as fxc, fuchsia_component_test as fxct,
    fuchsia_driver_test as _, fuchsia_zircon as zx,
};

const SUSPEND_DEVICE_TIMEOUT: zx::Duration = zx::Duration::from_seconds(5);

// The connection method is borrowed from system-activity-governor code.
async fn connect_to_control() -> Result<ffts::ControlProxy> {
    let service_dir = fxc::client::open_service::<ffts::ControlServiceMarker>()
        .expect("failed to open service dir");

    let mut watcher = fuchsia_fs::directory::Watcher::new(&service_dir)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to create watcher: {:?}", e))?;

    // Connect to the first suspend service instance that is discovered.
    let filename = loop {
        let next = watcher
            .try_next()
            .map_err(|e| anyhow::anyhow!("Failed to get next watch message: {e:?}"))
            .on_timeout(SUSPEND_DEVICE_TIMEOUT.after_now(), || {
                Err(anyhow::anyhow!("Timeout waiting for next watcher message."))
            })
            .await?;

        if let Some(watch_msg) = next {
            let filename = watch_msg.filename.as_path().to_str().unwrap().to_owned();
            if filename != "." {
                if watch_msg.event == fuchsia_fs::directory::WatchEvent::ADD_FILE
                    || watch_msg.event == fuchsia_fs::directory::WatchEvent::EXISTING
                {
                    break Ok(filename);
                }
            }
        } else {
            break Err(anyhow::anyhow!("Suspend service watcher returned None entry."));
        }
    }?;

    let svc_inst =
        fxc::client::connect_to_service_instance::<ffts::ControlServiceMarker>(filename.as_str())?;

    svc_inst
        .connect_to_control()
        .map_err(|e| anyhow::anyhow!("Failed to connect to control: {:?}", e))
}

#[fasync::run_singlethreaded(test)]
async fn test_suspend() -> Result<()> {
    let mut events = EventStream::open().await.unwrap();
    let collection = "suspend_inspect";
    let child_name = "suspend_linux";
    let url = "#meta/suspend_client.cm";

    // This moniker is relative to this test component. However, monikers in
    // test framework are relative to the **test root**. If you want to have
    // monikers relative to this component (which you probably do), then
    // the CML for routing `event_stream` **must** include a directive:
    // `scope: "#test_suite"`, or similar to inform the event subsystem.
    // If you don't do this, your code will not receive the events you expect.
    let moniker = format!("{collection}:{child_name}");

    // Starts the suspend driver test realm.
    let realm_proxy_control = fxc::client::connect_to_protocol::<ffth::RealmProxy_Marker>()
        .context("connecting to RealmProxy")?;

    let control_proxy = connect_to_control().await.context("while connecting to control")?;

    // If we want OK to be returned, we must set it explicitly. Otherwise an
    // error will be returned by default.
    control_proxy.set_suspend_enter_result(zx::Status::OK.into_raw()).await?;

    let state = control_proxy.get_state().await.context("while calling get_state")?;
    assert_eq!(0, state);

    {
        let instance =
            fxct::ScopedInstance::new_with_name(child_name.into(), collection.into(), url.into())
                .await?;

        info!("starting suspend_linux binary ... ");
        let _binder = instance.connect_to_binder().unwrap();

        EventMatcher::ok().moniker(&moniker).wait::<Started>(&mut events).await.unwrap();
        EventMatcher::ok().moniker(&moniker).wait::<Stopped>(&mut events).await.unwrap();
        info!("... and it stopped.");
    }

    // Linux program started, suspended, resumed, then stopped. The resulting
    // inspect should have been captured and ought to be available.
    let kernel_inspect = ArchiveReader::new()
        // See notes above for moniker.
        .select_all_for_moniker("test_suite/kernel")
        .with_minimum_schema_count(1)
        .snapshot::<Inspect>()
        .await?;
    print!("{:?}", kernel_inspect);
    assert_eq!(kernel_inspect.len(), 1);

    let state = control_proxy.get_state().await.context("while calling get_state")?;
    assert_eq!(1, state);

    Ok(())
}
