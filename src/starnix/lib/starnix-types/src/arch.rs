// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

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
}
