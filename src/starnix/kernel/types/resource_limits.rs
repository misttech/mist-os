// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::collections::HashMap;

use crate::types::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Resource(u32);

impl Resource {
    pub const NOFILE: Resource = Resource(RLIMIT_NOFILE);
    pub const STACK: Resource = Resource(RLIMIT_STACK);

    pub fn from_raw(raw: u32) -> Resource {
        Resource(raw)
    }
}

#[derive(Default)]
pub struct ResourceLimits {
    values: HashMap<Resource, rlimit>,
}

const INFINITE_LIMIT: rlimit =
    rlimit { rlim_cur: RLIM_INFINITY as u64, rlim_max: RLIM_INFINITY as u64 };

impl ResourceLimits {
    pub fn get(&self, resource: Resource) -> rlimit {
        *self.values.get(&resource).unwrap_or(&INFINITE_LIMIT)
    }

    pub fn set(&mut self, resource: Resource, value: rlimit) {
        self.values.insert(resource, value);
    }
}
