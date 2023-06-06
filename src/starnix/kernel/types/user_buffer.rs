// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use zerocopy::{AsBytes, FromBytes, FromZeroes};

use crate::{mm::PAGE_SIZE, types::*};
use once_cell::sync::Lazy;

/// Matches iovec_t.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, AsBytes, FromZeroes, FromBytes)]
#[repr(C)]
pub struct UserBuffer {
    pub address: UserAddress,
    pub length: usize,
}

pub static MAX_RW_COUNT: Lazy<usize> = Lazy::new(|| ((1 << 31) - *PAGE_SIZE) as usize);

impl UserBuffer {
    pub fn cap_buffers_to_max_rw_count(
        max_address: UserAddress,
        buffers: Vec<UserBuffer>,
    ) -> Result<(Vec<UserBuffer>, usize), Errno> {
        // Linux checks all buffers for plausibility, even those past the MAX_RW_COUNT threshold.
        if buffers
            .iter()
            .any(|buffer| buffer.address > max_address || buffer.length > max_address.ptr())
        {
            return error!(EFAULT);
        }
        let mut total = 0;
        let buffers = buffers
            .into_iter()
            .map_while(|mut buffer| {
                if total == *MAX_RW_COUNT {
                    None
                } else {
                    total = match total.checked_add(buffer.length).ok_or_else(|| errno!(EINVAL)) {
                        Ok(total) => total,
                        Err(e) => {
                            return Some(Err(e));
                        }
                    };
                    if total > *MAX_RW_COUNT {
                        buffer.length = total - *MAX_RW_COUNT;
                        total = *MAX_RW_COUNT;
                    }
                    Some(Ok(buffer))
                }
            })
            .collect::<Result<Vec<_>, Errno>>()?;
        Ok((buffers, total))
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
