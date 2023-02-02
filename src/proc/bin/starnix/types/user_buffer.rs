// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use zerocopy::{AsBytes, FromBytes};

use crate::types::*;

/// Matches iovec_t.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, AsBytes, FromBytes)]
#[repr(C)]
pub struct UserBuffer {
    pub address: UserAddress,
    pub length: usize,
}

impl UserBuffer {
    pub fn get_total_length(buffers: &[UserBuffer]) -> Result<usize, Errno> {
        let mut total: usize = 0;
        for buffer in buffers {
            total = total.checked_add(buffer.length).ok_or_else(|| errno!(EINVAL))?;
        }
        // The docs say we should return EINVAL if "the sum of the iov_len
        // values overflows an ssize_t value."
        if total > isize::MAX as usize {
            return error!(EINVAL);
        }
        Ok(total)
    }

    pub fn advance(&mut self, length: usize) -> Result<(), Errno> {
        self.address = self.address.checked_add(length).ok_or_else(|| errno!(EINVAL))?;
        self.length = self.length.checked_sub(length).ok_or_else(|| errno!(EINVAL))?;
        Ok(())
    }

    /// Returns whether the buffer address is 0 and its length is 0.
    pub fn is_null(&self) -> bool {
        self.address.is_null() && self.is_empty()
    }

    /// Returns whether the buffer length is 0.
    pub fn is_empty(&self) -> bool {
        self.length == 0
    }
}
