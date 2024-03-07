// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::collections::HashMap;

use starnix_uapi::{errno, error, errors::Errno};

/// Kernel state-machine-based implementation of asynchronous I/O.
/// See https://man7.org/linux/man-pages/man7/aio.7.html#NOTES
pub struct AioContext {
    max_operations: usize,
    pending_operations: usize,
}

impl AioContext {
    pub fn new(max_operations: usize) -> Self {
        AioContext { max_operations, pending_operations: 0 }
    }

    pub fn can_queue(&self) -> bool {
        self.pending_operations < self.max_operations
    }
}

pub struct AioContexts {
    contexts: HashMap<u64, AioContext>,
    next_id: u64,
}

impl Default for AioContexts {
    fn default() -> Self {
        AioContexts { contexts: HashMap::default(), next_id: 1 }
    }
}

impl AioContexts {
    pub fn setup_context(&mut self, max_operations: u32) -> Result<u64, Errno> {
        let id = self.next_id;
        self.next_id = id.checked_add(1).ok_or_else(|| errno!(ENOMEM))?;
        self.contexts.insert(id, AioContext::new(max_operations as usize));
        Ok(id)
    }

    pub fn get_context(&self, id: u64) -> Option<&AioContext> {
        self.contexts.get(&id)
    }

    pub fn destroy_context(&mut self, id: u64) -> Result<(), Errno> {
        if let Some(_) = self.contexts.remove(&id) {
            Ok(())
        } else {
            error!(EINVAL)
        }
    }
}
