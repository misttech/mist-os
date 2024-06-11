// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use event_listener::Event;
use std::ops::Deref;

/// Same as Event but notifies when dropped.
pub struct DropEvent(Event);

impl Drop for DropEvent {
    fn drop(&mut self) {
        self.0.notify(usize::MAX);
    }
}

impl Deref for DropEvent {
    type Target = Event;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DropEvent {
    pub fn new() -> Self {
        Self(Event::new())
    }
}
