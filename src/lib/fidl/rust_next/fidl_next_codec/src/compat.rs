// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{TakeFrom, WireI64};

impl<T: fidl::Timeline> TakeFrom<WireI64> for fidl::Instant<T, fidl::NsUnit> {
    #[inline]
    fn take_from(from: &WireI64) -> Self {
        Self::from_nanos(**from)
    }
}

impl<T: fidl::Timeline> TakeFrom<WireI64> for fidl::Instant<T, fidl::TicksUnit> {
    #[inline]
    fn take_from(from: &WireI64) -> Self {
        Self::from_raw(**from)
    }
}
