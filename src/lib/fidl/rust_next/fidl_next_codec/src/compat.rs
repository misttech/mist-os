// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{FromWire, FromWireRef, WireI64};

impl<T: fidl::Timeline> FromWire<WireI64> for fidl::Instant<T, fidl::NsUnit> {
    #[inline]
    fn from_wire(wire: WireI64) -> Self {
        Self::from_wire_ref(&wire)
    }
}

impl<T: fidl::Timeline> FromWireRef<WireI64> for fidl::Instant<T, fidl::NsUnit> {
    #[inline]
    fn from_wire_ref(wire: &WireI64) -> Self {
        Self::from_nanos(**wire)
    }
}

impl<T: fidl::Timeline> FromWire<WireI64> for fidl::Instant<T, fidl::TicksUnit> {
    #[inline]
    fn from_wire(wire: WireI64) -> Self {
        Self::from_wire_ref(&wire)
    }
}

impl<T: fidl::Timeline> FromWireRef<WireI64> for fidl::Instant<T, fidl::TicksUnit> {
    #[inline]
    fn from_wire_ref(wire: &WireI64) -> Self {
        Self::from_raw(**wire)
    }
}
