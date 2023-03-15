// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
use {
    crate::cap::{AnyCapability, Capability},
    std::collections::HashMap,
};

pub type Key = String;

/// A capability that represents a dictionary of capabilities.
#[derive(Debug)]
pub struct Dict {
    pub entries: HashMap<Key, AnyCapability>,
}

impl Capability for Dict {}

impl Dict {
    pub fn new() -> Self {
        Dict { entries: HashMap::new() }
    }
}
