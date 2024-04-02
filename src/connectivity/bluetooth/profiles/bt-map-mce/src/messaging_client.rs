// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fuchsia_sync::Mutex;
use std::collections::VecDeque;

pub struct WatchAccessorRequest(
    // TODO(https://fxbug.dev/332390332): Remove or explain #[allow(dead_code)].
    #[allow(dead_code)] pub fidl_fuchsia_bluetooth_map::MessagingClientWatchAccessorResponder,
);

pub struct MessagingClient {
    accessor_requests: Mutex<VecDeque<WatchAccessorRequest>>,
    // TODO(b/323425471): keep track of connected peers.
}

impl MessagingClient {
    pub fn new() -> Self {
        MessagingClient { accessor_requests: Mutex::new(VecDeque::new()) }
    }

    pub fn queue_watch_accessor(&self, request: WatchAccessorRequest) {
        self.accessor_requests.lock().push_back(request);
    }

    // TODO(b/323425607): implement method for when a peer is connected.
}

// TODO(b/323425471): implement MceDevice.
