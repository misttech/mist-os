// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod validation;

use alloc::{sync::Arc, vec::Vec};

use derivative::Derivative;
use net_types::ip::{GenericOverIp, Ip};
use packet_formats::ip::IpExt;

use crate::matchers::PacketMatcher;

/// Ancillary information that is attached to filtering state during
/// installation to allow, for example, Bindings to report specific errors, or
/// Core to store associated state while converting filtering state.
pub trait ValidationInfo {
    /// Debug info that is attached to each uninstalled routine.
    type UninstalledRoutine<I: IpExt, DeviceClass>;
    /// Debug info that is attached to each rule.
    type Rule;
}

impl ValidationInfo for () {
    type UninstalledRoutine<I: IpExt, DeviceClass> = ();
    type Rule = ();
}

/// The action to take on a packet.
#[derive(Derivative)]
#[derivative(
    Clone(bound = "V::Rule: Clone, DeviceClass: Clone"),
    Debug(bound = "DeviceClass: core::fmt::Debug")
)]
pub enum Action<I: IpExt, DeviceClass, V: ValidationInfo> {
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
    Jump(UninstalledRoutine<I, DeviceClass, V>),
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
#[derivative(Clone(bound = ""), Debug(bound = "DeviceClass: core::fmt::Debug"))]
pub struct UninstalledRoutine<I: IpExt, DeviceClass, V: ValidationInfo>(
    pub Arc<UninstalledRoutineInner<I, DeviceClass, V>>,
);

impl<I: IpExt, DeviceClass, V: ValidationInfo> UninstalledRoutine<I, DeviceClass, V>
where
    V::UninstalledRoutine<I, DeviceClass>: Default,
{
    /// Creates a new uninstalled routine with the provided contents.
    pub fn new(rules: Vec<Rule<I, DeviceClass, V>>) -> Self {
        Self(Arc::new(UninstalledRoutineInner {
            routine: Routine { rules },
            validation_info: V::UninstalledRoutine::default(),
        }))
    }
}

/// A [`Routine`] that is not installed in a particular hook, and therefore is
/// only run if jumped to from another routine.
#[derive(Derivative)]
#[derivative(Debug(bound = "DeviceClass: core::fmt::Debug"))]
pub struct UninstalledRoutineInner<I: IpExt, DeviceClass, V: ValidationInfo> {
    /// The contents of this uninstalled routine.
    pub routine: Routine<I, DeviceClass, V>,
    /// Opaque information about this routine for use when validating and
    /// converting state provided by Bindings into Core filtering state. This is
    /// only used when installing filtering state and is zero-sized for
    /// validated state.
    #[derivative(Debug = "ignore")]
    pub(crate) validation_info: V::UninstalledRoutine<I, DeviceClass>,
}

/// A set of criteria (matchers) and a resultant action to take if a given
/// packet matches.
#[derive(Derivative, GenericOverIp)]
#[generic_over_ip(I, Ip)]
#[derivative(
    Clone(bound = "V::Rule: Clone, DeviceClass: Clone"),
    Debug(bound = "DeviceClass: core::fmt::Debug")
)]
pub struct Rule<I: IpExt, DeviceClass, V: ValidationInfo> {
    /// The criteria that a packet must match for the action to be executed.
    pub matcher: PacketMatcher<I, DeviceClass>,
    /// The action to take on a matching packet.
    pub action: Action<I, DeviceClass, V>,
    /// Opaque information about this rule for use when validating and
    /// converting state provided by Bindings into Core filtering state. This is
    /// only used when installing filtering state and is zero-sized for
    /// validated state.
    #[derivative(Debug = "ignore")]
    pub validation_info: V::Rule,
}

/// A sequence of [`Rule`]s.
#[derive(Derivative, GenericOverIp)]
#[generic_over_ip(I, Ip)]
#[derivative(
    Clone(bound = "V::Rule: Clone, DeviceClass: Clone"),
    Debug(bound = "DeviceClass: core::fmt::Debug")
)]
pub struct Routine<I: IpExt, DeviceClass, V: ValidationInfo> {
    /// The rules to be executed in order.
    pub rules: Vec<Rule<I, DeviceClass, V>>,
}

/// A particular entry point for packet processing in which filtering routines
/// are installed.
#[derive(Derivative, GenericOverIp)]
#[generic_over_ip(I, Ip)]
#[derivative(Default(bound = ""), Debug(bound = "DeviceClass: core::fmt::Debug"))]
pub struct Hook<I: IpExt, DeviceClass, V: ValidationInfo> {
    /// The routines to be executed in order.
    pub routines: Vec<Routine<I, DeviceClass, V>>,
}

/// Routines that perform ordinary IP filtering.
#[derive(Derivative)]
#[derivative(Default(bound = ""), Debug(bound = "DeviceClass: core::fmt::Debug"))]
pub struct IpRoutines<I: IpExt, DeviceClass, V: ValidationInfo> {
    /// Occurs for incoming traffic before a routing decision has been made.
    pub ingress: Hook<I, DeviceClass, V>,
    /// Occurs for incoming traffic that is destined for the local host.
    pub local_ingress: Hook<I, DeviceClass, V>,
    /// Occurs for incoming traffic that is destined for another node.
    pub forwarding: Hook<I, DeviceClass, V>,
    /// Occurs for locally-generated traffic before a final routing decision has
    /// been made.
    pub local_egress: Hook<I, DeviceClass, V>,
    /// Occurs for all outgoing traffic after a routing decision has been made.
    pub egress: Hook<I, DeviceClass, V>,
}

/// Routines that can perform NAT.
///
/// Note that NAT routines are only executed *once* for a given connection, for
/// the first packet in the flow.
#[derive(Derivative)]
#[derivative(Default(bound = ""), Debug(bound = "DeviceClass: core::fmt::Debug"))]
// TODO(https://fxbug.dev/318717702): implement NAT.
#[allow(dead_code)]
pub struct NatRoutines<I: IpExt, DeviceClass, V: ValidationInfo> {
    /// Occurs for incoming traffic before a routing decision has been made.
    pub ingress: Hook<I, DeviceClass, V>,
    /// Occurs for incoming traffic that is destined for the local host.
    pub local_ingress: Hook<I, DeviceClass, V>,
    /// Occurs for locally-generated traffic before a final routing decision has
    /// been made.
    pub local_egress: Hook<I, DeviceClass, V>,
    /// Occurs for all outgoing traffic after a routing decision has been made.
    pub egress: Hook<I, DeviceClass, V>,
}

/// IP version-specific filtering state.
#[derive(Derivative, GenericOverIp)]
#[generic_over_ip(I, Ip)]
#[derivative(Default(bound = ""), Debug(bound = "DeviceClass: core::fmt::Debug"))]
pub struct State<I: IpExt, DeviceClass, V: ValidationInfo> {
    /// Routines that perform IP filtering.
    pub ip_routines: IpRoutines<I, DeviceClass, V>,
    /// Routines that perform IP filtering and NAT.
    pub nat_routines: NatRoutines<I, DeviceClass, V>,
}
