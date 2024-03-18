// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod validation;

use alloc::{sync::Arc, vec::Vec};
use core::{
    fmt::Debug,
    hash::{Hash, Hasher},
};

use derivative::Derivative;
use net_types::ip::{GenericOverIp, Ip};
use packet_formats::ip::IpExt;

use crate::matchers::PacketMatcher;

/// The action to take on a packet.
#[derive(Derivative)]
#[derivative(
    Clone(bound = "DeviceClass: Clone, RuleInfo: Clone"),
    Debug(bound = "DeviceClass: Debug")
)]
pub enum Action<I: IpExt, DeviceClass, RuleInfo> {
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
    Jump(UninstalledRoutine<I, DeviceClass, RuleInfo>),
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
/// A handle to a [`Routine`] that is not installed in a particular hook, and
/// therefore is only run if jumped to from another routine.
#[derive(Derivative)]
#[derivative(Clone(bound = ""), Debug(bound = "DeviceClass: Debug"))]
pub struct UninstalledRoutine<I: IpExt, DeviceClass, RuleInfo>(
    pub(crate) Arc<Routine<I, DeviceClass, RuleInfo>>,
);

impl<I: IpExt, DeviceClass, RuleInfo> UninstalledRoutine<I, DeviceClass, RuleInfo> {
    /// Creates a new uninstalled routine with the provided contents.
    pub fn new(rules: Vec<Rule<I, DeviceClass, RuleInfo>>) -> Self {
        Self(Arc::new(Routine { rules }))
    }
}

impl<I: IpExt, DeviceClass, RuleInfo> PartialEq for UninstalledRoutine<I, DeviceClass, RuleInfo> {
    fn eq(&self, other: &Self) -> bool {
        let Self(lhs) = self;
        let Self(rhs) = other;
        Arc::ptr_eq(lhs, rhs)
    }
}

impl<I: IpExt, DeviceClass, RuleInfo> Eq for UninstalledRoutine<I, DeviceClass, RuleInfo> {}

impl<I: IpExt, DeviceClass, RuleInfo> Hash for UninstalledRoutine<I, DeviceClass, RuleInfo> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        let Self(inner) = self;
        Arc::as_ptr(inner).hash(state)
    }
}

/// A set of criteria (matchers) and a resultant action to take if a given
/// packet matches.
#[derive(Derivative, GenericOverIp)]
#[generic_over_ip(I, Ip)]
#[derivative(
    Clone(bound = "DeviceClass: Clone, RuleInfo: Clone"),
    Debug(bound = "DeviceClass: Debug")
)]
pub struct Rule<I: IpExt, DeviceClass, RuleInfo> {
    /// The criteria that a packet must match for the action to be executed.
    pub matcher: PacketMatcher<I, DeviceClass>,
    /// The action to take on a matching packet.
    pub action: Action<I, DeviceClass, RuleInfo>,
    /// Opaque information about this rule for use when validating and
    /// converting state provided by Bindings into Core filtering state. This is
    /// only used when installing filtering state, and allows Core to report to
    /// Bindings which rule caused a particular error. It is zero-sized for
    /// validated state.
    #[derivative(Debug = "ignore")]
    pub validation_info: RuleInfo,
}

/// A sequence of [`Rule`]s.
#[derive(Derivative, GenericOverIp)]
#[generic_over_ip(I, Ip)]
#[derivative(
    Clone(bound = "DeviceClass: Clone, RuleInfo: Clone"),
    Debug(bound = "DeviceClass: Debug")
)]
pub struct Routine<I: IpExt, DeviceClass, RuleInfo> {
    /// The rules to be executed in order.
    pub rules: Vec<Rule<I, DeviceClass, RuleInfo>>,
}

/// A particular entry point for packet processing in which filtering routines
/// are installed.
#[derive(Derivative, GenericOverIp)]
#[generic_over_ip(I, Ip)]
#[derivative(Default(bound = ""), Debug(bound = "DeviceClass: Debug"))]
pub struct Hook<I: IpExt, DeviceClass, RuleInfo> {
    /// The routines to be executed in order.
    pub routines: Vec<Routine<I, DeviceClass, RuleInfo>>,
}

/// Routines that perform ordinary IP filtering.
#[derive(Derivative)]
#[derivative(Default(bound = ""), Debug(bound = "DeviceClass: Debug"))]
pub struct IpRoutines<I: IpExt, DeviceClass, RuleInfo> {
    /// Occurs for incoming traffic before a routing decision has been made.
    pub ingress: Hook<I, DeviceClass, RuleInfo>,
    /// Occurs for incoming traffic that is destined for the local host.
    pub local_ingress: Hook<I, DeviceClass, RuleInfo>,
    /// Occurs for incoming traffic that is destined for another node.
    pub forwarding: Hook<I, DeviceClass, RuleInfo>,
    /// Occurs for locally-generated traffic before a final routing decision has
    /// been made.
    pub local_egress: Hook<I, DeviceClass, RuleInfo>,
    /// Occurs for all outgoing traffic after a routing decision has been made.
    pub egress: Hook<I, DeviceClass, RuleInfo>,
}

/// Routines that can perform NAT.
///
/// Note that NAT routines are only executed *once* for a given connection, for
/// the first packet in the flow.
#[derive(Derivative)]
#[derivative(Default(bound = ""), Debug(bound = "DeviceClass: Debug"))]
pub struct NatRoutines<I: IpExt, DeviceClass, RuleInfo> {
    /// Occurs for incoming traffic before a routing decision has been made.
    pub ingress: Hook<I, DeviceClass, RuleInfo>,
    /// Occurs for incoming traffic that is destined for the local host.
    pub local_ingress: Hook<I, DeviceClass, RuleInfo>,
    /// Occurs for locally-generated traffic before a final routing decision has
    /// been made.
    pub local_egress: Hook<I, DeviceClass, RuleInfo>,
    /// Occurs for all outgoing traffic after a routing decision has been made.
    pub egress: Hook<I, DeviceClass, RuleInfo>,
}

/// IP version-specific filtering state.
#[derive(Derivative, GenericOverIp)]
#[generic_over_ip(I, Ip)]
#[derivative(Default(bound = ""), Debug(bound = "DeviceClass: Debug"))]
pub struct State<I: IpExt, DeviceClass, RuleInfo> {
    /// Routines that perform IP filtering.
    pub ip_routines: IpRoutines<I, DeviceClass, RuleInfo>,
    /// Routines that perform IP filtering and NAT.
    pub nat_routines: NatRoutines<I, DeviceClass, RuleInfo>,
}
