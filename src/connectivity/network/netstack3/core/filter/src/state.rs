// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use alloc::{sync::Arc, vec::Vec};

use derivative::Derivative;
use net_types::ip::{GenericOverIp, Ip};
use packet_formats::ip::IpExt;

use crate::matchers::PacketMatcher;

/// The action to take on a packet.
#[derive(Debug, Clone)]
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
    /// Jump from the current routine to the specified uninstalled routine.
    Jump(UninstalledRoutine<I, DeviceClass>),
    // TODO(https://fxbug.dev/318718273): implement jumping and returning.
    /// Stop evaluation of the current routine and return to the calling routine
    /// (the routine from which the current routine was jumped), continuing
    /// evaluation at the next rule.
    ///
    /// If invoked in an installed routine, equivalent to `Accept`, given
    /// packets are accepted by default in the absence of any matching rules.
    Return,
}

// TODO(https://fxbug.dev/318718273): implement jumping and returning.
/// A [`Routine`] that is not installed in a particular hook, and therefore is
/// only run if jumped to from another routine.
#[derive(Debug, Clone)]
pub struct UninstalledRoutine<I: IpExt, DeviceClass>(pub Arc<Routine<I, DeviceClass>>);

/// A set of criteria (matchers) and a resultant action to take if a given
/// packet matches.
#[derive(Debug, Clone, GenericOverIp)]
#[generic_over_ip(I, Ip)]
pub struct Rule<I: IpExt, DeviceClass> {
    /// The criteria that a packet must match for the action to be executed.
    pub matcher: PacketMatcher<I, DeviceClass>,
    /// The action to take on a matching packet.
    pub action: Action<I, DeviceClass>,
}

/// A sequence of [`Rule`]s.
#[derive(Debug, Clone, GenericOverIp)]
#[generic_over_ip(I, Ip)]
pub struct Routine<I: IpExt, DeviceClass> {
    /// The rules to be executed in order.
    pub rules: Vec<Rule<I, DeviceClass>>,
}

/// A particular entry point for packet processing in which filtering routines
/// are installed.
#[derive(Derivative, Debug, GenericOverIp)]
#[generic_over_ip(I, Ip)]
#[derivative(Default(bound = ""))]
pub struct Hook<I: IpExt, DeviceClass> {
    /// The routines to be executed in order.
    pub routines: Vec<Routine<I, DeviceClass>>,
}

/// Routines that perform ordinary IP filtering.
#[derive(Derivative, Debug)]
#[derivative(Default(bound = ""))]
pub struct IpRoutines<I: IpExt, DeviceClass> {
    /// Occurs for incoming traffic before a routing decision has been made.
    pub ingress: Hook<I, DeviceClass>,
    /// Occurs for incoming traffic that is destined for the local host.
    pub local_ingress: Hook<I, DeviceClass>,
    /// Occurs for incoming traffic that is destined for another node.
    pub forwarding: Hook<I, DeviceClass>,
    /// Occurs for locally-generated traffic before a final routing decision has
    /// been made.
    pub local_egress: Hook<I, DeviceClass>,
    /// Occurs for all outgoing traffic after a routing decision has been made.
    pub egress: Hook<I, DeviceClass>,
}

/// Routines that can perform NAT.
///
/// Note that NAT routines are only executed *once* for a given connection, for
/// the first packet in the flow.
#[derive(Derivative, Debug)]
#[derivative(Default(bound = ""))]
// TODO(https://fxbug.dev/318717702): implement NAT.
#[allow(dead_code)]
pub struct NatRoutines<I: IpExt, DeviceClass> {
    /// Occurs for incoming traffic before a routing decision has been made.
    pub ingress: Hook<I, DeviceClass>,
    /// Occurs for incoming traffic that is destined for the local host.
    pub local_ingress: Hook<I, DeviceClass>,
    /// Occurs for locally-generated traffic before a final routing decision has
    /// been made.
    pub local_egress: Hook<I, DeviceClass>,
    /// Occurs for all outgoing traffic after a routing decision has been made.
    pub egress: Hook<I, DeviceClass>,
}

/// IP version-specific filtering state.
#[derive(Derivative, Debug, GenericOverIp)]
#[generic_over_ip(I, Ip)]
#[derivative(Default(bound = ""))]
pub struct State<I: IpExt, DeviceClass> {
    /// Routines that perform IP filtering.
    pub ip_routines: IpRoutines<I, DeviceClass>,
    /// Routines that perform IP filtering and NAT.
    pub nat_routines: NatRoutines<I, DeviceClass>,
}
