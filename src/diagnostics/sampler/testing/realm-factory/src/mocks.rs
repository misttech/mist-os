// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use component_id_index::{Index, InstanceId};
use fidl::persist;
use fidl_fuchsia_component_internal::ComponentIdIndex;
use futures::channel::mpsc;
use futures::lock::Mutex;
use futures::{SinkExt, StreamExt, TryStreamExt};
use moniker::Moniker;
use std::str::FromStr;
use std::sync::Arc;
use {
    fidl_fuchsia_hardware_power_statecontrol as reboot,
    fidl_fuchsia_mockrebootcontroller as controller, fuchsia_async as fasync,
};

/// Test data for moniker <-> ID file.
/// This will be sent to Sampler as though it were coming from Product Assembly.
pub(crate) fn id_file_vmo() -> zx::Vmo {
    let mut index = Index::default();
    let id: InstanceId =
        InstanceId::from_str("1111222233334444111111111111111111111111111111111111111111111111")
            .unwrap();
    index.insert(Moniker::try_from("integer_42").unwrap(), id).unwrap();
    let id: InstanceId =
        InstanceId::from_str("2222222233334444111111111111111111111111111111111111111111112222")
            .unwrap();
    index.insert(Moniker::try_from("not_listed_1").unwrap(), id).unwrap();
    let index_fidl: ComponentIdIndex = ComponentIdIndex::from(index);
    let index_bytes = persist(&index_fidl).unwrap();
    let vmo = zx::Vmo::create(index_bytes.len() as u64).unwrap();
    vmo.write(&index_bytes, 0).unwrap();
    vmo
}

pub fn serve_reboot_server(
    mut stream: reboot::RebootMethodsWatcherRegisterRequestStream,
    mut proxy_sender: mpsc::Sender<reboot::RebootWatcherProxy>,
) {
    fasync::Task::spawn(async move {
        while let Some(req) = stream.try_next().await.unwrap() {
            match req {
                // TODO(https://fxbug.dev/385742868): Remove this once the
                // method is deleted from the FIDL API.
                reboot::RebootMethodsWatcherRegisterRequest::Register {
                    watcher: _,
                    control_handle: _,
                } => {
                    panic!("`Register` is deprecated, use `RegisterWatcher` instead");
                }
                // TODO(https://fxbug.dev/385742868): Remove this once the
                // method is deleted from the FIDL API.
                reboot::RebootMethodsWatcherRegisterRequest::RegisterWithAck {
                    watcher: _,
                    responder: _,
                } => {
                    panic!("`RegisterWithAck` is deprecated, use `RegisterWatcher` instead");
                }
                reboot::RebootMethodsWatcherRegisterRequest::RegisterWatcher {
                    watcher,
                    responder,
                } => {
                    proxy_sender.send(watcher.into_proxy()).await.unwrap();
                    responder.send().unwrap();
                }
            }
        }
    })
    .detach();
}

pub fn serve_reboot_controller(
    mut stream: controller::MockRebootControllerRequestStream,
    proxy_receiver: Arc<Mutex<mpsc::Receiver<reboot::RebootWatcherProxy>>>,
) {
    fasync::Task::spawn(async move {
        while let Some(req) = stream.try_next().await.unwrap() {
            let proxy = proxy_receiver.lock().await.next().await.unwrap();
            match req {
                controller::MockRebootControllerRequest::TriggerReboot { responder } => match proxy
                    .on_reboot(&reboot::RebootOptions {
                        reasons: Some(vec![reboot::RebootReason2::UserRequest]),
                        __source_breaking: fidl::marker::SourceBreaking,
                    })
                    .await
                {
                    Err(_) => {
                        responder.send(Err(controller::RebootError::ClientError)).unwrap();
                    }
                    Ok(()) => {
                        responder.send(Ok(())).unwrap();
                    }
                },
                controller::MockRebootControllerRequest::CrashRebootChannel { responder } => {
                    drop(proxy);
                    responder.send(Ok(())).unwrap();
                }
            }
        }
    })
    .detach();
}
