// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::{convert::Infallible as Never, marker::PhantomData};

use lock_order::Unlocked;
use lock_order::{impl_lock_after, relation::LockAfter};
use net_types::ip::{Ipv4, Ipv6};

pub(crate) struct IpState<I>(PhantomData<I>, Never);

pub(crate) enum DeviceLayerStateOrigin {}
pub(crate) enum DeviceLayerState {}
pub(crate) struct EthernetDeviceIpState<I>(PhantomData<I>, Never);
pub(crate) enum EthernetDeviceStaticState {}
pub(crate) enum EthernetDeviceDynamicState {}

pub(crate) enum EthernetIpv4Arp {}
pub(crate) enum EthernetIpv6Nud {}

pub(crate) enum LoopbackRxQueue {}
pub(crate) enum LoopbackRxDequeue {}

impl LockAfter<Unlocked> for DeviceLayerState {}
impl LockAfter<Unlocked> for LoopbackRxQueue {}
impl_lock_after!(LoopbackRxQueue => LoopbackRxDequeue);

impl_lock_after!(DeviceLayerState => EthernetDeviceIpState<Ipv4>);
// TODO(https://fxbug.dev/120973): Double-check that locking IPv4 ethernet state
// before IPv6 is correct and won't interfere with dual-stack sockets.
impl_lock_after!(EthernetDeviceIpState<Ipv4> => EthernetDeviceIpState<Ipv6>);
impl_lock_after!(DeviceLayerState => EthernetDeviceStaticState);
impl_lock_after!(DeviceLayerState => EthernetDeviceDynamicState);
impl_lock_after!(DeviceLayerState => EthernetIpv6Nud);
impl_lock_after!(DeviceLayerState => EthernetIpv4Arp);
