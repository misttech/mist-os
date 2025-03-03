// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use starnix_uapi::user_address::{MultiArchUserRef, UserAddress, UserRef};

#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchWidth {
    #[default]
    Arch64,
    #[cfg(feature = "arch32")]
    Arch32,
}

impl ArchWidth {
    pub fn is_arch32(&self) -> bool {
        cfg_if::cfg_if! {
            if #[cfg(feature = "arch32")] {
                self == &Self::Arch32
            } else {
                false
            }
        }
    }

    pub fn make_user_ref<T64, T32, A: Into<u64>>(&self, address: A) -> MultiArchUserRef<T64, T32> {
        if self.is_arch32() {
            MultiArchUserRef::<T64, T32>::from_32(UserRef::<T32>::from(UserAddress::from(
                address.into(),
            )))
        } else {
            MultiArchUserRef::<T64, T32>::from(UserRef::<T64>::from(UserAddress::from(
                address.into(),
            )))
        }
    }
}
