// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::device::Info as DeviceInfo;
use anyhow::{anyhow, Context, Error};
use async_utils::event::Event as AsyncEvent;
use async_utils::hanging_get::client::HangingGetStream;
use fidl_fuchsia_audio_device as fadevice;
use fuchsia_async::Task;
use futures::{lock::Mutex, StreamExt};
use std::collections::BTreeMap;
use std::sync::Arc;

pub struct Registry {
    devices: Arc<Mutex<BTreeMap<fadevice::TokenId, DeviceInfo>>>,
    devices_initialized: AsyncEvent,
    _watch_devices_task: Task<Result<(), Error>>,
}

impl Registry {
    pub fn new(proxy: fadevice::RegistryProxy) -> Self {
        let devices = Arc::new(Mutex::new(BTreeMap::new()));
        let devices_initialized = AsyncEvent::new();
        let watch_devices_task =
            Task::spawn(watch_devices(proxy, devices.clone(), devices_initialized.clone()));
        Self { devices, devices_initialized, _watch_devices_task: watch_devices_task }
    }

    /// Returns information about the device with the given `token_id`.
    ///
    /// Returns None if there is no device with the given ID.
    pub async fn get(&self, token_id: fadevice::TokenId) -> Option<DeviceInfo> {
        self.devices_initialized.wait().await;
        self.devices.lock().await.get(&token_id).cloned()
    }

    /// Returns information about all devices in the registry.
    pub async fn get_all(&self) -> BTreeMap<fadevice::TokenId, DeviceInfo> {
        self.devices_initialized.wait().await;
        self.devices.lock().await.clone()
    }
}

/// Watches devices added to and removed from the registry and updates
/// `devices` with the current state.
///
/// Signals `devices_initialized` when `devices` is populated with the initial
/// set of devices.
async fn watch_devices(
    proxy: fadevice::RegistryProxy,
    devices: Arc<Mutex<BTreeMap<fadevice::TokenId, DeviceInfo>>>,
    devices_initialized: AsyncEvent,
) -> Result<(), Error> {
    let mut devices_initialized = Some(devices_initialized);

    let mut devices_added_stream =
        HangingGetStream::new(proxy.clone(), fadevice::RegistryProxy::watch_devices_added);
    let mut device_removed_stream =
        HangingGetStream::new(proxy, fadevice::RegistryProxy::watch_device_removed);

    loop {
        futures::select! {
            added = devices_added_stream.select_next_some() => {
                let response = added
                    .context("failed to call WatchDevicesAdded")?
                    .map_err(|err| anyhow!("failed to watch for added devices: {:?}", err))?;
                let added_devices = response.devices.ok_or_else(|| anyhow!("missing devices"))?;

                let mut devices = devices.lock().await;
                for new_device in added_devices.into_iter() {
                    let token_id = new_device.token_id.ok_or_else(|| anyhow!("device info missing token_id"))?;
                    let _ = devices.insert(token_id, DeviceInfo::from(new_device));
                }

                if let Some(devices_initialized) = devices_initialized.take() {
                    devices_initialized.signal();
                }
            },
            removed = device_removed_stream.select_next_some() => {
                let response = removed
                    .context("failed to call WatchDeviceRemoved")?
                    .map_err(|err| anyhow!("failed to watch for removed device: {:?}", err))?;
                let token_id = response.token_id.ok_or_else(|| anyhow!("missing token_id"))?;
                let mut devices = devices.lock().await;
                let _ = devices.remove(&token_id);
            }
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use async_utils::hanging_get::server::{HangingGet, Publisher};
    use fidl::endpoints::spawn_local_stream_handler;

    type AddedResponse = fadevice::RegistryWatchDevicesAddedResponse;
    type AddedResponder = fadevice::RegistryWatchDevicesAddedResponder;
    type AddedNotifyFn = Box<dyn Fn(&AddedResponse, AddedResponder) -> bool>;
    type AddedPublisher = Publisher<AddedResponse, AddedResponder, AddedNotifyFn>;

    type RemovedResponse = fadevice::RegistryWatchDeviceRemovedResponse;
    type RemovedResponder = fadevice::RegistryWatchDeviceRemovedResponder;
    type RemovedNotifyFn = Box<dyn Fn(&RemovedResponse, RemovedResponder) -> bool>;
    type RemovedPublisher = Publisher<RemovedResponse, RemovedResponder, RemovedNotifyFn>;

    fn serve_registry(
        initial_devices: Vec<fadevice::Info>,
    ) -> (fadevice::RegistryProxy, AddedPublisher, RemovedPublisher) {
        let initial_added_response =
            AddedResponse { devices: Some(initial_devices), ..Default::default() };
        let watch_devices_added_notify: AddedNotifyFn =
            Box::new(|response, responder: AddedResponder| {
                responder.send(Ok(response)).expect("failed to send response");
                true
            });
        let added_broker = HangingGet::new(initial_added_response, watch_devices_added_notify);
        let added_publisher = added_broker.new_publisher();

        let watch_device_removed_notify: RemovedNotifyFn =
            Box::new(|response, responder: RemovedResponder| {
                responder.send(Ok(response)).expect("failed to send response");
                true
            });
        let removed_broker = HangingGet::new_unknown_state(watch_device_removed_notify);
        let removed_publisher = removed_broker.new_publisher();

        let added_broker = Arc::new(Mutex::new(added_broker));
        let removed_broker = Arc::new(Mutex::new(removed_broker));

        let proxy = spawn_local_stream_handler(move |request| {
            let added_broker = added_broker.clone();
            let removed_broker = removed_broker.clone();
            async move {
                let added_subscriber = added_broker.lock().await.new_subscriber();
                let removed_subscriber = removed_broker.lock().await.new_subscriber();
                match request {
                    fadevice::RegistryRequest::WatchDevicesAdded { responder } => {
                        added_subscriber.register(responder).unwrap()
                    }
                    fadevice::RegistryRequest::WatchDeviceRemoved { responder } => {
                        removed_subscriber.register(responder).unwrap()
                    }
                    _ => unimplemented!(),
                }
            }
        })
        .unwrap();

        (proxy, added_publisher, removed_publisher)
    }

    #[fuchsia::test]
    async fn test_get() {
        let devices = vec![fadevice::Info { token_id: Some(1), ..Default::default() }];
        let (registry_proxy, _added_publisher, _removed_publisher) = serve_registry(devices);
        let registry = Registry::new(registry_proxy);

        assert!(registry.get(1).await.is_some());
        assert!(registry.get(2).await.is_none());
    }
}
