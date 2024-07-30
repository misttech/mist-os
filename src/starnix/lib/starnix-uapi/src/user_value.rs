// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use zerocopy::{AsBytes, FromBytes, FromZeros, NoCell};

/// A value Starnix has received from userspace.
///
/// Typically, these values are received in syscall arguments and need to be validated before they
/// can be used directly. For example, integers need to be checked for overflow during arithmetical
/// operation.
#[derive(Clone, Copy, Eq, PartialEq, AsBytes, FromZeros, FromBytes, NoCell)]
#[repr(transparent)]
pub struct UserValue<T: Copy + Eq + AsBytes + FromZeros + FromBytes + NoCell>(T);

impl<T: Copy + Eq + AsBytes + FromZeros + FromBytes + NoCell> UserValue<T> {
    /// Create a UserValue from a raw value provided by userspace.
    pub fn from_raw(raw: T) -> Self {
        Self(raw)
    }

    /// The raw value that the user provided.
    pub fn raw(&self) -> T {
        self.0
    }

    /// Attempt to convert this value into another type.
    pub fn try_into<U: TryFrom<T>>(self) -> Result<U, <U as TryFrom<T>>::Error> {
        U::try_from(self.0)
    }
}

impl<T: Copy + Eq + AsBytes + FromZeros + FromBytes + NoCell> From<T> for UserValue<T> {
    fn from(value: T) -> Self {
        Self::from_raw(value)
    }
}

impl<T: PartialEq<T> + Copy + Eq + AsBytes + FromZeros + FromBytes + NoCell> PartialEq<T>
    for UserValue<T>
{
    fn eq(&self, other: &T) -> bool {
        self.0 == *other
    }
}
