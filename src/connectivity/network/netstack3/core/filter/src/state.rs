// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use alloc::{sync::Arc, vec::Vec};

use derivative::Derivative;
use net_types::ip::{GenericOverIp, Ip};
use packet_formats::ip::IpExt;

use crate::matchers::PacketMatcher;

/// The action to take on a packet.
#[derive(Debug)]
pub enum Action<I: IpExt, DeviceClass> {
    /// Accept the packet.
    ///
    /// This is a terminal action for the current routine, i.e. no further rules
    /// will be evaluated for this packet in the routine in which this rule is
    /// installed. Subsequent routines on the same hook will still be evaluated.
    Accept,
    /// Drop the packet.
    ///
    /// This is a terminal action for the current hook, i.e. no further rules
    /// will be evaluated for this packet, even in other routines on the same
    /// hook.
    Drop,
    // TODO(https://fxbug.dev/318718273): implement jumping and returning.
    #[allow(dead_code)]
    /// Jump from the current routine to the specified uninstalled routine.
    Jump(UninstalledRoutine<I, DeviceClass>),
    // TODO(https://fxbug.dev/318718273): implement jumping and returning.
    #[allow(dead_code)]
    /// Stop evaluation of the current routine and return to the calling routine
    /// (the routine from which the current routine was jumped), continuing
    /// evaluation at the next rule.
    ///
    /// If invoked in an installed routine, equivalent to `Accept`, given
    /// packets are accepted by default in the absence of any matching rules.
    Return,
}

#[derive(Debug)]
pub struct UninstalledRoutine<I: IpExt, DeviceClass>(Arc<Routine<I, DeviceClass>>);

/// A set of criteria (matchers) and a resultant action to take if a given
/// packet matches.
#[derive(Debug, GenericOverIp)]
#[generic_over_ip(I, Ip)]
pub struct Rule<I: IpExt, DeviceClass> {
    pub(crate) matcher: PacketMatcher<I, DeviceClass>,
    pub(crate) action: Action<I, DeviceClass>,
}

/// A sequence of [`Rule`]s.
#[derive(Debug, GenericOverIp)]
#[generic_over_ip(I, Ip)]
pub struct Routine<I: IpExt, DeviceClass> {
    pub(crate) rules: Vec<Rule<I, DeviceClass>>,
}

/// A particular entry point for packet processing in which filtering routines
/// are installed.
#[derive(Derivative, Debug, GenericOverIp)]
#[generic_over_ip(I, Ip)]
#[derivative(Default(bound = ""))]
pub struct Hook<I: IpExt, DeviceClass> {
    pub(crate) routines: Vec<Routine<I, DeviceClass>>,
}

#[derive(Derivative, Debug)]
#[derivative(Default(bound = ""))]
pub struct IpRoutines<I: IpExt, DeviceClass> {
    pub(crate) ingress: Hook<I, DeviceClass>,
    pub(crate) local_ingress: Hook<I, DeviceClass>,
    pub(crate) forwarding: Hook<I, DeviceClass>,
    pub(crate) local_egress: Hook<I, DeviceClass>,
    pub(crate) egress: Hook<I, DeviceClass>,
}

#[derive(Derivative, Debug)]
#[derivative(Default(bound = ""))]
// TODO(https://fxbug.dev/318717702): implement NAT.
#[allow(dead_code)]
pub struct NatRoutines<I: IpExt, DeviceClass> {
    ingress: Hook<I, DeviceClass>,
    local_ingress: Hook<I, DeviceClass>,
    local_egress: Hook<I, DeviceClass>,
    egress: Hook<I, DeviceClass>,
}

/// IP version-specific filtering state.
#[derive(Derivative, Debug, GenericOverIp)]
#[generic_over_ip(I, Ip)]
#[derivative(Default(bound = ""))]
pub struct State<I: IpExt, DeviceClass> {
    pub(crate) ip_routines: IpRoutines<I, DeviceClass>,
    // TODO(https://fxbug.dev/318717702): implement NAT.
    #[allow(dead_code)]
    pub(crate) nat_routines: NatRoutines<I, DeviceClass>,
}
