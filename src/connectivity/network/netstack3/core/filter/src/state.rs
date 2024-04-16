// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod validation;

use alloc::{format, string::ToString as _, sync::Arc, vec::Vec};
use core::{
    fmt::Debug,
    hash::{Hash, Hasher},
};

use derivative::Derivative;
use net_types::ip::{GenericOverIp, Ip};
use netstack3_base::{Inspectable, Inspector as _};
use packet_formats::ip::IpExt;

use crate::{conntrack, matchers::PacketMatcher, ValidRoutines};

/// The action to take on a packet.
#[derive(Derivative)]
#[derivative(
    Clone(bound = "DeviceClass: Clone, RuleInfo: Clone"),
    Debug(bound = "DeviceClass: Debug")
)]
pub enum Action<I: IpExt, DeviceClass, RuleInfo> {
    /// Accept the packet.
    ///
    /// This is a terminal action for the current *installed* routine, i.e. no
    /// further rules will be evaluated for this packet in the installed routine
    /// (or any subroutines) in which this rule is installed. Subsequent
    /// routines installed on the same hook will still be evaluated.
    Accept,
    /// Drop the packet.
    ///
    /// This is a terminal action for the current hook, i.e. no further rules
    /// will be evaluated for this packet, even in other routines on the same
    /// hook.
    Drop,
    /// Jump from the current routine to the specified uninstalled routine.
    Jump(UninstalledRoutine<I, DeviceClass, RuleInfo>),
    /// Stop evaluation of the current routine and return to the calling routine
    /// (the routine from which the current routine was jumped), continuing
    /// evaluation at the next rule.
    ///
    /// If invoked in an installed routine, equivalent to `Accept`, given
    /// packets are accepted by default in the absence of any matching rules.
    Return,
}

impl<I: IpExt, DeviceClass: Debug> Inspectable for Action<I, DeviceClass, ()> {
    fn record<Inspector: netstack3_base::Inspector>(&self, inspector: &mut Inspector) {
        let value = match self {
            Self::Accept | Self::Drop | Self::Return => format!("{self:?}"),
            Self::Jump(UninstalledRoutine { routine: _, id }) => {
                format!("Jump(UninstalledRoutine({id:?}))")
            }
        };
        inspector.record_string("action", value);
    }
}

/// A handle to a [`Routine`] that is not installed in a particular hook, and
/// therefore is only run if jumped to from another routine.
#[derive(Derivative)]
#[derivative(Clone(bound = ""), Debug(bound = "DeviceClass: Debug"))]
pub struct UninstalledRoutine<I: IpExt, DeviceClass, RuleInfo> {
    pub(crate) routine: Arc<Routine<I, DeviceClass, RuleInfo>>,
    id: usize,
}

impl<I: IpExt, DeviceClass, RuleInfo> UninstalledRoutine<I, DeviceClass, RuleInfo> {
    /// Creates a new uninstalled routine with the provided contents.
    pub fn new(rules: Vec<Rule<I, DeviceClass, RuleInfo>>, id: usize) -> Self {
        Self { routine: Arc::new(Routine { rules }), id }
    }

    /// Returns the inner routine.
    pub fn get(&self) -> &Routine<I, DeviceClass, RuleInfo> {
        &*self.routine
    }
}

impl<I: IpExt, DeviceClass, RuleInfo> PartialEq for UninstalledRoutine<I, DeviceClass, RuleInfo> {
    fn eq(&self, other: &Self) -> bool {
        let Self { routine: lhs, id: _ } = self;
        let Self { routine: rhs, id: _ } = other;
        Arc::ptr_eq(lhs, rhs)
    }
}

impl<I: IpExt, DeviceClass, RuleInfo> Eq for UninstalledRoutine<I, DeviceClass, RuleInfo> {}

impl<I: IpExt, DeviceClass, RuleInfo> Hash for UninstalledRoutine<I, DeviceClass, RuleInfo> {
    fn hash<H: Hasher>(&self, state: &mut H) {
        let Self { routine, id: _ } = self;
        Arc::as_ptr(routine).hash(state)
    }
}

impl<I: IpExt, DeviceClass: Debug> Inspectable for UninstalledRoutine<I, DeviceClass, ()> {
    fn record<Inspector: netstack3_base::Inspector>(&self, inspector: &mut Inspector) {
        let Self { routine, id } = self;
        inspector.record_child(&id.to_string(), |inspector| {
            inspector.delegate_inspectable(&**routine);
        });
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

impl<I: IpExt, DeviceClass: Debug> Inspectable for Rule<I, DeviceClass, ()> {
    fn record<Inspector: netstack3_base::Inspector>(&self, inspector: &mut Inspector) {
        let Self { matcher, action, validation_info: () } = self;
        inspector.record_child("matchers", |inspector| {
            let PacketMatcher {
                in_interface,
                out_interface,
                src_address,
                dst_address,
                transport_protocol,
            } = matcher;

            fn record_matcher<Inspector: netstack3_base::Inspector, M: Debug>(
                inspector: &mut Inspector,
                name: &str,
                matcher: &Option<M>,
            ) {
                if let Some(matcher) = matcher {
                    inspector.record_string(name, format!("{matcher:?}"))
                }
            }

            record_matcher(inspector, "in_interface", in_interface);
            record_matcher(inspector, "out_interface", out_interface);
            record_matcher(inspector, "src_address", src_address);
            record_matcher(inspector, "dst_address", dst_address);
            record_matcher(inspector, "transport_protocol", transport_protocol);
        });
        inspector.delegate_inspectable(action);
    }
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

impl<I: IpExt, DeviceClass: Debug> Inspectable for Routine<I, DeviceClass, ()> {
    fn record<Inspector: netstack3_base::Inspector>(&self, inspector: &mut Inspector) {
        let Self { rules } = self;
        for rule in rules {
            inspector.record_unnamed_child(|inspector| inspector.delegate_inspectable(rule));
        }
    }
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

impl<I: IpExt, DeviceClass: Debug> Inspectable for Hook<I, DeviceClass, ()> {
    fn record<Inspector: netstack3_base::Inspector>(&self, inspector: &mut Inspector) {
        let Self { routines } = self;
        for routine in routines {
            inspector.record_unnamed_child(|inspector| {
                inspector.delegate_inspectable(routine);
            });
        }
    }
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

/// Data stored in [`conntrack::Connection`] that is only needed by filtering.
#[derive(Default)]
pub struct ConntrackExternalData {}

/// IP version-specific filtering routine state.
#[derive(Derivative, GenericOverIp)]
#[generic_over_ip(I, Ip)]
#[derivative(Default(bound = ""), Debug(bound = "DeviceClass: Debug"))]
pub struct Routines<I: IpExt, DeviceClass, RuleInfo> {
    /// Routines that perform IP filtering.
    pub ip: IpRoutines<I, DeviceClass, RuleInfo>,
    /// Routines that perform IP filtering and NAT.
    pub nat: NatRoutines<I, DeviceClass, RuleInfo>,
}

/// IP version-specific filtering state.
#[derive(Derivative)]
#[derivative(Default(bound = ""))]
pub struct State<I: IpExt, DeviceClass> {
    /// Routines used for filtering packets that are installed on hooks.
    pub installed_routines: ValidRoutines<I, DeviceClass>,
    /// Routines that are only executed if jumped to from other routines.
    ///
    /// Jump rules refer to their targets by holding a reference counted pointer
    /// to the inner routine; we hold this index of all uninstalled routines
    /// that have any references in order to report them in inspect data.
    pub(crate) uninstalled_routines: Vec<UninstalledRoutine<I, DeviceClass, ()>>,
    /// Connection tracking state.
    #[allow(dead_code)]
    pub(crate) conntrack: conntrack::Table<I, ConntrackExternalData>,
}

impl<I: IpExt, DeviceClass: Debug> Inspectable for State<I, DeviceClass> {
    fn record<Inspector: netstack3_base::Inspector>(&self, inspector: &mut Inspector) {
        let Self { installed_routines, uninstalled_routines, conntrack: _ } = self;
        // TODO(https://fxbug.dev/318717702): when we implement NAT, report NAT
        // routines in inspect data.
        let Routines { ip, nat: _ } = installed_routines.get();
        let IpRoutines { ingress, local_ingress, forwarding, local_egress, egress } = ip;

        inspector.record_child("ingress", |inspector| inspector.delegate_inspectable(ingress));
        inspector.record_child("local_ingress", |inspector| {
            inspector.delegate_inspectable(local_ingress)
        });
        inspector
            .record_child("forwarding", |inspector| inspector.delegate_inspectable(forwarding));
        inspector
            .record_child("local_egress", |inspector| inspector.delegate_inspectable(local_egress));
        inspector.record_child("egress", |inspector| inspector.delegate_inspectable(egress));

        inspector.record_child("uninstalled", |inspector| {
            for routine in uninstalled_routines {
                inspector.delegate_inspectable(routine);
            }
        });
    }
}

/// A trait for interacting with the pieces of packet metadata that are
/// important for filtering.
pub trait FilterIpMetadata<I: IpExt> {
    /// Removes the conntrack connection, if it exists.
    fn take_conntrack_connection(
        &mut self,
    ) -> Option<conntrack::Connection<I, ConntrackExternalData>>;

    /// Puts a new conntrack connection into the metadata struct, returning the
    /// previous value.
    fn replace_conntrack_connection(
        &mut self,
        conn: conntrack::Connection<I, ConntrackExternalData>,
    ) -> Option<conntrack::Connection<I, ConntrackExternalData>>;
}
