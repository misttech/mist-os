// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::prelude_internal::*;
use ot::types::packets_and_bytes::PacketsAndBytes;

/// This structure represents border routing counters.
///
/// Functional equivalent of [`otsys::otBorderRoutingCounters`](crate::otsys::otBorderRoutingCounters).
#[derive(Debug, Default, Clone)]
#[repr(transparent)]
pub struct BorderRoutingCounters(pub otBorderRoutingCounters);

impl_ot_castable!(BorderRoutingCounters, otBorderRoutingCounters);

impl BorderRoutingCounters {
    /// Counters for inbound unicast packets.
    pub fn inbound_unicast(&self) -> &PacketsAndBytes {
        PacketsAndBytes::ref_from_ot_ref(&self.0.mInboundUnicast)
    }

    /// Counters for inbound multicast packets.
    pub fn inbound_multicast(&self) -> &PacketsAndBytes {
        PacketsAndBytes::ref_from_ot_ref(&self.0.mInboundMulticast)
    }

    /// Counters for outbound unicast packets.
    pub fn outbound_unicast(&self) -> &PacketsAndBytes {
        PacketsAndBytes::ref_from_ot_ref(&self.0.mOutboundUnicast)
    }

    /// Counters for outbound multicast packets.
    pub fn outbound_multicast(&self) -> &PacketsAndBytes {
        PacketsAndBytes::ref_from_ot_ref(&self.0.mOutboundMulticast)
    }
}
