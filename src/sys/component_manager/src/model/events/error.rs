// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {cm_types::Name, thiserror::Error};

#[derive(Debug, Error, Clone)]
pub enum EventsError {
    #[error("capability_requested event streams cannot be taken twice")]
    CapabilityRequestedStreamTaken,

    #[error("Model not available")]
    ModelNotAvailable,

    #[error("Instance destroyed")]
    InstanceDestroyed,

    #[error("Registry not found")]
    RegistryNotFound,

    #[error("Event {:?} appears more than once in a subscription request", event_name)]
    DuplicateEvent { event_name: Name },

    #[error("Events not allowed for subscription {:?}", names)]
    NotAvailable { names: Vec<Name> },
}

impl EventsError {
    pub fn duplicate_event(event_name: Name) -> Self {
        Self::DuplicateEvent { event_name }
    }

    pub fn not_available(names: Vec<Name>) -> Self {
        Self::NotAvailable { names }
    }
}
