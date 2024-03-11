// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use fidl_fuchsia_starnix_device as fstardevice;
use fuchsia_async as fasync;
use fuchsia_zircon as zx;
use starnix_sync::Mutex;
use std::{collections::HashMap, ops::DerefMut, sync::Arc};
use zx::{AsHandleRef, HandleBased, Koid};

pub struct SyncFenceRegistry {
    sync_fence_map: Arc<Mutex<HashMap<Koid, zx::Event>>>,
    task_holder: Arc<Mutex<fasync::TaskGroup>>,
}

// Implementation of the sync framework described at:
// https://source.android.com/docs/core/graphics/sync
impl SyncFenceRegistry {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            sync_fence_map: Arc::new(Mutex::new(HashMap::new())),
            task_holder: Arc::new(Mutex::new(fasync::TaskGroup::new())),
        })
    }

    pub fn create_sync_fences(
        &self,
        num_fences: u32,
    ) -> (Vec<fstardevice::SyncFenceKey>, Vec<zx::Event>) {
        let mut fence_keys: Vec<fstardevice::SyncFenceKey> = vec![];
        let mut events: Vec<zx::Event> = vec![];

        for _ in 0..num_fences {
            let (local_token, client_token) = zx::EventPair::create();
            let koid = local_token.as_handle_ref().get_koid().expect("Failed to get_koid");
            fence_keys.push(fstardevice::SyncFenceKey { value: client_token });
            let event = zx::Event::create();
            let dup_event =
                event.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("Failed to dup event.");
            events.push(event);
            self.sync_fence_map.lock().deref_mut().insert(koid, dup_event);

            let sync_fence_map_clone = self.sync_fence_map.clone();
            let task = fasync::Task::spawn(async move {
                fasync::OnSignals::new(&local_token, zx::Signals::EVENTPAIR_PEER_CLOSED)
                    .await
                    .expect("waiting for EVENTPAIR_PEER_CLOSED failed");
                sync_fence_map_clone.lock().remove(&koid);
            });
            self.task_holder.lock().add(task);
        }

        (fence_keys, events)
    }

    pub fn register_signaled_event(
        &self,
        fence_key: fstardevice::SyncFenceKey,
        client_event: zx::Event,
    ) {
        let koid = fence_key.value.as_handle_ref().basic_info().unwrap().related_koid;
        let sync_fence_map_lock = self.sync_fence_map.lock();
        let event = sync_fence_map_lock.get(&koid);
        if event.is_none() {
            client_event
                .signal_handle(zx::Signals::NONE, zx::Signals::EVENT_SIGNALED)
                .expect("Failed to signal event");
            return;
        }

        // We should depend on a vector of events being signaled when this class is extended.
        let dup_event =
            event.unwrap().duplicate_handle(zx::Rights::SAME_RIGHTS).expect("Failed to dup event.");
        let task = fasync::Task::spawn(async move {
            fasync::OnSignals::new(&dup_event, zx::Signals::EVENT_SIGNALED)
                .await
                .expect("waiting for EVENT_SIGNALED failed");
            client_event
                .signal_handle(zx::Signals::NONE, zx::Signals::EVENT_SIGNALED)
                .expect("Failed to signal event");
        });
        self.task_holder.lock().add(task);
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use test_case::test_case;

    #[test_case(1u32)]
    #[test_case(3u32)]
    #[::fuchsia::test]
    async fn create_sync_fence(num_fences: u32) {
        let registry = SyncFenceRegistry::new();

        let (fence_keys, events) = registry.create_sync_fences(num_fences);
        assert_eq!(num_fences, fence_keys.len() as u32);
        assert_eq!(num_fences, events.len() as u32);
        for fence_key in fence_keys {
            assert!(!fence_key.value.is_invalid_handle());
        }
        for event in events {
            assert!(!event.is_invalid_handle());
        }
    }

    #[::fuchsia::test]
    async fn drop_sync_fence() {
        let map = Arc::new(Mutex::new(HashMap::new()));
        let task_holder = Arc::new(Mutex::new(fasync::TaskGroup::new()));
        let registry =
            SyncFenceRegistry { sync_fence_map: map.clone(), task_holder: task_holder.clone() };

        let (mut fence_keys, _events) = registry.create_sync_fences(1u32);
        assert_eq!(1, map.lock().len());

        std::mem::drop(fence_keys.remove(0));
        // Run all queued tasks before checking state.
        let queued_tasks =
            std::mem::replace(task_holder.lock().deref_mut(), fasync::TaskGroup::new());
        queued_tasks.join().await;
        assert_eq!(0, map.lock().len());
    }

    #[::fuchsia::test]
    async fn register_signaled_event() {
        let registry = SyncFenceRegistry::new();

        let (mut fence_keys, events) = registry.create_sync_fences(1u32);
        assert_eq!(1, events.len());
        assert_eq!(1, fence_keys.len());

        let signaled_event = zx::Event::create();
        registry.register_signaled_event(
            fence_keys.remove(0),
            signaled_event.duplicate_handle(zx::Rights::SAME_RIGHTS).expect("Failed to dup event."),
        );
        assert_eq!(
            signaled_event.wait_handle(zx::Signals::EVENT_SIGNALED, zx::Time::ZERO),
            Err(zx::Status::TIMED_OUT),
        );

        events[0]
            .signal_handle(zx::Signals::NONE, zx::Signals::EVENT_SIGNALED)
            .expect("Failed to signal event");
        fasync::OnSignals::new(&signaled_event, zx::Signals::EVENT_SIGNALED)
            .await
            .expect("waiting for EVENT_SIGNALED failed");
    }
}
