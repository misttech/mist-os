// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{lock::Mutex, types::*};
use bitflags::bitflags;
use std::sync::Arc;

pub type SealFlagsHandle = Arc<Mutex<SealFlags>>;

bitflags! {
    /// The flags that are used to seal a memory-backed file descriptor (memfd)
    /// and prevent it from being modified.
    pub struct SealFlags: u32 {
      const FUTURE_WRITE = uapi::F_SEAL_FUTURE_WRITE;
      const WRITE = uapi::F_SEAL_WRITE;
      const GROW = uapi::F_SEAL_GROW;
      const SHRINK = uapi::F_SEAL_SHRINK;
      const SEAL = uapi::F_SEAL_SEAL;
    }
}

impl SealFlags {
    /// Add a new seal to the current set, if allowed.
    pub fn try_add_seal(&mut self, flags: SealFlags) -> Result<(), Errno> {
        if self.contains(SealFlags::SEAL) {
            // More seals cannot be added
            return error!(EPERM);
        }

        // TODO: If adding the WRITE seal, check that the memfd does not have any shared writable mappings.

        self.insert(flags);

        Ok(())
    }

    /// Fails with EPERM if the current seal flags contain any of the given `flags`.
    pub fn check_not_present(&self, flags: SealFlags) -> Result<(), Errno> {
        if self.intersects(flags) {
            error!(EPERM)
        } else {
            Ok(())
        }
    }
}
