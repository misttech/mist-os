// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::convert::Infallible as Never;
use core::fmt::Debug;

use net_types::ip::{GenericOverIp, Ip, Ipv4, Ipv6};

/// `Ip` extension trait to assist in defining [`NextHop`].
pub trait BroadcastIpExt: Ip {
    /// A marker type carried by the [`NextHop::Broadcast`] variant to indicate
    /// that it is uninhabited for IPv6.
    type BroadcastMarker: Debug + Copy + Clone + PartialEq + Eq + Send + Sync;
}

impl BroadcastIpExt for Ipv4 {
    type BroadcastMarker = ();
}

impl BroadcastIpExt for Ipv6 {
    type BroadcastMarker = Never;
}

/// Wrapper struct to provide a convenient [`GenericOverIp`] impl for use
/// with [`BroadcastIpExt::BroadcastMarker`].
#[derive(GenericOverIp)]
#[generic_over_ip(I, Ip)]
pub struct WrapBroadcastMarker<I: BroadcastIpExt>(pub I::BroadcastMarker);
