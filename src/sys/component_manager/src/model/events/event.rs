// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {hooks::Event as ComponentEvent, moniker::ExtendedMoniker};

/// Created for a particular component event.
/// Contains the Event that occurred along with a means to resume/unblock the component manager.
#[derive(Debug)]
pub struct Event {
    /// The event itself.
    pub event: ComponentEvent,

    /// The scope where this event comes from. This can be seen as a superset of the
    /// `event.target_moniker` itself given that the events might have been offered from an
    /// ancestor realm.
    pub scope_moniker: ExtendedMoniker,
}
