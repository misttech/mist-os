// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::fetcher::{FetchCommand, Fetcher};
use fuchsia_async as fasync;
use fuchsia_sync::Mutex;

use persistence_config::{Config, ServiceName, Tag, TagConfig};
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

// This contains the logic to decide which tags to fetch at what times. It contains the state of
// each tag (when last fetched, whether currently queued). When a request arrives via FIDL, it's
// sent here and results in requests queued to the Fetcher.

#[derive(Clone)]
pub(crate) struct Scheduler {
    scope: fasync::ScopeHandle,
    // This is a global lock. Scheduler only does schedule() which is synchronous and quick.
    state: Arc<Mutex<State>>,
}

struct State {
    fetcher: Fetcher,
    services: HashMap<ServiceName, HashMap<Tag, TagState>>,
}

struct TagState {
    backoff: zx::MonotonicDuration,
    state: FetchState,
    last_fetched: zx::MonotonicInstant,
}

impl Scheduler {
    pub(crate) fn new(scope: fasync::ScopeHandle, fetcher: Fetcher, config: &Config) -> Self {
        let mut services = HashMap::new();
        for (service, tags) in config {
            let mut tag_states = HashMap::new();
            for (tag, tag_config) in tags {
                let TagConfig { min_seconds_between_fetch, .. } = tag_config;
                let backoff = zx::MonotonicDuration::from_seconds(*min_seconds_between_fetch);
                let tag_state = TagState {
                    backoff,
                    state: FetchState::Idle,
                    last_fetched: zx::MonotonicInstant::INFINITE_PAST,
                };
                tag_states.insert(tag.clone(), tag_state);
            }
            services.insert(service.clone(), tag_states);
        }
        Scheduler { scope, state: Arc::new(Mutex::new(State { fetcher, services })) }
    }

    /// Gets a service name and a list of valid tags, and queues any fetches that are not already
    /// pending. Updates the last-fetched time on any tag it queues, setting it equal to the later
    /// of the current time and the time the fetch becomes possible.
    pub(crate) fn schedule(&self, service: &ServiceName, tags: Vec<Tag>) {
        if tags.is_empty() {
            return;
        }

        // Every tag we process should use the same Now
        let now = zx::MonotonicInstant::get();
        let mut state = self.state.lock();
        let Some(service_info) = state.services.get_mut(service) else {
            return;
        };

        // Filter tags that need to be fetch now from those that need to be
        // fetched later. Group later tags by their next_fetch time using a
        // b-tree, making it efficient to iterate over these batches in
        // order of next_fetch time.
        let mut now_tags = vec![];
        let mut later_tags: BTreeMap<zx::MonotonicInstant, Vec<Tag>> = BTreeMap::new();
        for tag in tags {
            let Some(tag_state) = service_info.get_mut(&tag) else {
                return;
            };
            if matches!(tag_state.state, FetchState::Pending) {
                continue;
            }
            if tag_state.last_fetched + tag_state.backoff <= now {
                now_tags.push(tag);
                tag_state.last_fetched = now;
            } else {
                let next_fetch = tag_state.last_fetched + tag_state.backoff;
                tag_state.last_fetched = next_fetch;
                tag_state.state = FetchState::Pending;
                later_tags.entry(next_fetch).or_default().push(tag);
            }
        }
        if !now_tags.is_empty() {
            state.fetcher.send(FetchCommand { service: service.clone(), tags: now_tags });
        }

        // Schedule tags that need to be fetch later.
        while let Some((next_fetch, tags)) = later_tags.pop_first() {
            let state = self.state.clone();
            let service = service.clone();
            self.scope.spawn(async move {
                fasync::Timer::new(next_fetch).await;
                let mut state = state.lock();
                let Some(tag_states) = state.services.get_mut(&service.clone()) else {
                    return;
                };
                for tag in tags.iter() {
                    tag_states.get_mut(tag).unwrap().state = FetchState::Idle;
                }
                state.fetcher.send(FetchCommand { service: service.clone(), tags });
            });
        }
    }
}

/// FetchState tells whether a tag is currently waiting to be dispatched or not. If it is, then
/// another request to fetch that tag should cause no change. If it's not waiting, then it can
/// either be fetched immediately (in which case its state stays Idle, but the last-fetched time
/// will be updated to Now) or it will be queued (in which case its state is Pending and its
/// last-fetched time will be set forward to the time it's going to be fetched).
enum FetchState {
    Pending,
    Idle,
}
