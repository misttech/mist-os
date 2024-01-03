// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::fetcher::{FetchCommand, Fetcher};
use fuchsia_async::{self as fasync, TaskGroup};
use fuchsia_sync::Mutex;
use fuchsia_zircon as zx;
use persistence_config::{Config, ServiceName, Tag, TagConfig};
use std::{collections::HashMap, sync::Arc};

// This contains the logic to decide which tags to fetch at what times. It contains the state of
// each tag (when last fetched, whether currently queued). When a request arrives via FIDL, it's
// sent here and results in requests queued to the Fetcher.

#[derive(Clone)]
pub(crate) struct Scheduler {
    // This is a global lock. Scheduler only does schedule() which is synchronous and quick.
    state: Arc<Mutex<State>>,
}

struct State {
    fetcher: Fetcher,
    services: HashMap<ServiceName, HashMap<Tag, TagState>>,
    tasks: TaskGroup,
}

struct TagState {
    backoff: zx::Duration,
    state: FetchState,
    last_fetched: zx::Time,
}

impl Scheduler {
    pub(crate) fn new(fetcher: Fetcher, config: &Config) -> Self {
        let mut services = HashMap::new();
        for (service, tags) in config {
            let mut tag_states = HashMap::new();
            for (tag, tag_config) in tags {
                let TagConfig { min_seconds_between_fetch, .. } = tag_config;
                let backoff = zx::Duration::from_seconds(*min_seconds_between_fetch);
                let tag_state = TagState {
                    backoff,
                    state: FetchState::Idle,
                    last_fetched: zx::Time::INFINITE_PAST,
                };
                tag_states.insert(tag.clone(), tag_state);
            }
            services.insert(service.clone(), tag_states);
        }
        let state = State { fetcher, services, tasks: TaskGroup::new() };
        Scheduler { state: Arc::new(Mutex::new(state)) }
    }

    /// Gets a service name and a list of valid tags, and queues any fetches that are not already
    /// pending. Updates the last-fetched time on any tag it queues, setting it equal to the later
    /// of the current time and the time the fetch becomes possible.
    pub(crate) fn schedule(&self, service: &ServiceName, tags: Vec<Tag>) {
        // Every tag we process should use the same Now
        let now = zx::Time::get_monotonic();
        let mut state = self.state.lock();
        let Some(service_info) = state.services.get_mut(service) else {
            return;
        };

        let mut now_tags = vec![];
        let mut later_tags: Vec<(zx::Time, Tag)> = vec![];
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
                later_tags.push((next_fetch, tag));
            }
        }
        if !now_tags.is_empty() {
            let _ = state.fetcher.send(FetchCommand { service: service.clone(), tags: now_tags });
        }
        // later_tags may not all be fetchable at the same time. Batch the ones that are.
        later_tags.sort_by(|a, b| a.0.cmp(&b.0));
        while !later_tags.is_empty() {
            // This is N^2 but N will be too small to matter.
            let first_time = later_tags[0].0;
            let mut first_tags = vec![];
            let mut remaining_tags = vec![];
            for (next_fetch, tag) in later_tags {
                if next_fetch == first_time {
                    first_tags.push(tag);
                } else {
                    remaining_tags.push((next_fetch, tag));
                }
            }
            later_tags = remaining_tags;
            self.enqueue(
                &mut state,
                first_time,
                FetchCommand { service: service.clone(), tags: first_tags },
            );
        }
    }

    fn enqueue(&self, state: &mut State, time: zx::Time, command: FetchCommand) {
        let this = self.clone();
        let mut fetcher = state.fetcher.clone();
        state.tasks.spawn(async move {
            fasync::Timer::new(time).await;
            {
                let mut state = this.state.lock();
                let Some(tag_states) = state.services.get_mut(&command.service) else {
                    return;
                };
                for tag in command.tags.iter() {
                    tag_states.get_mut(tag).unwrap().state = FetchState::Idle;
                }
            }
            let _ = fetcher.send(command);
        });
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
