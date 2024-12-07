// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod validation;

use alloc::format;
use alloc::string::ToString as _;
use alloc::sync::Arc;
use alloc::vec::Vec;
use core::fmt::Debug;
use core::hash::{Hash, Hasher};
use core::num::NonZeroU16;
use core::ops::RangeInclusive;

use derivative::Derivative;
use net_types::ip::{GenericOverIp, Ip};
use netstack3_base::{CoreTimerContext, Inspectable, InspectableValue, Inspector as _};
use packet_formats::ip::IpExt;

use crate::conntrack;
use crate::context::{FilterBindingsContext, FilterBindingsTypes};
use crate::logic::nat::NatConfig;
use crate::logic::FilterTimerId;
use crate::matchers::PacketMatcher;
use crate::state::validation::ValidRoutines;

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
    /// Redirect the packet to a local socket without changing the packet header
    /// in any way.
    ///
    /// This is a terminal action for the current hook, i.e. no further rules
    /// will be evaluated for this packet, even in other routines on the same
    /// hook. However, note that this does not preclude actions on *other* hooks
    /// from having an effect on this packet; for example, a packet that hits
    /// TransparentProxy in INGRESS could still be dropped in LOCAL_INGRESS.
    ///
    /// This action is only valid in the INGRESS hook. This action is also only
    /// valid in a rule that ensures the presence of a TCP or UDP header by
    /// matching on the transport protocol, so that the packet can be properly
    /// dispatched.
    ///
    /// Also note that transparently proxied packets will only be delivered to
    /// sockets with the transparent socket option enabled.
    TransparentProxy(TransparentProxy<I>),
    /// A special case of destination NAT (DNAT) that redirects the packet to
    /// the local host.
    ///
    /// This is a terminal action for all NAT routines on the current hook. The
    /// packet is redirected by rewriting the destination IP address to one
    /// owned by the ingress interface (if operating on incoming traffic in
    /// INGRESS) or the loopback address (if operating on locally-generated
    /// traffic in LOCAL_EGRESS). If this rule is installed on INGRESS and no IP
    /// address is assigned to the incoming interface, the packet is dropped.
    ///
    /// As with all DNAT actions, this action is only valid in the INGRESS and
    /// LOCAL_EGRESS hooks. If a destination port is specified, this action is
    /// only valid in a rule that ensures the presence of a TCP or UDP header by
    /// matching on the transport protocol, so that the destination port can be
    /// rewritten.
    ///
    /// This is analogous to the `redirect` statement in Netfilter.
    Redirect {
        /// The optional range of destination ports used to rewrite the packet.
        ///
        /// If specified, the destination port of the packet will be rewritten
        /// to some randomly chosen port in the range. If absent, the
        /// destination port of the packet will not be rewritten.
        dst_port: Option<RangeInclusive<NonZeroU16>>,
    },
    /// A special case of source NAT (SNAT) that reassigns the source IP address
    /// of the packet to an address that is assigned to the outgoing interface.
    ///
    /// This is a terminal action for all NAT routines on the current hook. If
    /// no address is assigned to the outgoing interface, the packet will be
    /// dropped.
    ///
    /// This action is only valid in the EGRESS hook. If a source port range is
    /// specified, this action is only valid in a rule that ensures the presence
    /// of a TCP or UDP header by matching on the transport protocol, so that
    /// the source port can be rewritten.
    ///
    /// This is analogous to the `masquerade` statement in Netfilter.
    Masquerade {
        /// The optional range of source ports used to rewrite the packet.
        ///
        /// The source port will be rewritten if necessary to ensure the
        /// packet's flow does not conflict with an existing tracked connection.
        /// Note that the source port may be rewritten whether or not this range
        /// is specified.
        ///
        /// If specified, this overrides the default behavior and restricts the
        /// range of possible values to which the source port can be rewritten.
        src_port: Option<RangeInclusive<NonZeroU16>>,
    },
}

/// Transparently intercept the packet and deliver it to a local socket without
/// changing the packet header.
///
/// When a local address is specified, it is the bound address of the local
/// socket to redirect the packet to. When absent, the destination IP address of
/// the packet is used for local delivery.
///
/// When a local port is specified, it is the bound port of the local socket to
/// redirect the packet to. When absent, the destination port of the packet is
/// used for local delivery.
#[derive(Debug, Clone)]
#[allow(missing_docs)]
pub enum TransparentProxy<I: IpExt> {
    LocalAddr(I::Addr),
    LocalPort(NonZeroU16),
    LocalAddrAndPort(I::Addr, NonZeroU16),
}

impl<I: IpExt, DeviceClass: Debug> Inspectable for Action<I, DeviceClass, ()> {
    fn record<Inspector: netstack3_base::Inspector>(&self, inspector: &mut Inspector) {
        let value = match self {
            Self::Accept
            | Self::Drop
            | Self::Return
            | Self::TransparentProxy(_)
            | Self::Redirect { .. }
            | Self::Masquerade { .. } => {
                format!("{self:?}")
            }
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

            fn record_matcher<Inspector: netstack3_base::Inspector, M: InspectableValue>(
                inspector: &mut Inspector,
                name: &str,
                matcher: &Option<M>,
            ) {
                if let Some(matcher) = matcher {
                    inspector.record_inspectable_value(name, matcher);
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
        inspector.record_usize("rules", rules.len());
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
        inspector.record_usize("routines", routines.len());
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

impl<I: IpExt, DeviceClass: Debug> Inspectable for IpRoutines<I, DeviceClass, ()> {
    fn record<Inspector: netstack3_base::Inspector>(&self, inspector: &mut Inspector) {
        let Self { ingress, local_ingress, forwarding, local_egress, egress } = self;

        inspector.record_child("ingress", |inspector| inspector.delegate_inspectable(ingress));
        inspector.record_child("local_ingress", |inspector| {
            inspector.delegate_inspectable(local_ingress)
        });
        inspector
            .record_child("forwarding", |inspector| inspector.delegate_inspectable(forwarding));
        inspector
            .record_child("local_egress", |inspector| inspector.delegate_inspectable(local_egress));
        inspector.record_child("egress", |inspector| inspector.delegate_inspectable(egress));
    }
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

impl<I: IpExt, DeviceClass, RuleInfo> NatRoutines<I, DeviceClass, RuleInfo> {
    pub(crate) fn contains_rules(&self) -> bool {
        let Self { ingress, local_ingress, local_egress, egress } = self;

        let hook_contains_rules =
            |hook: &Hook<_, _, _>| hook.routines.iter().any(|routine| !routine.rules.is_empty());
        hook_contains_rules(&ingress)
            || hook_contains_rules(&local_ingress)
            || hook_contains_rules(&local_egress)
            || hook_contains_rules(&egress)
    }
}

impl<I: IpExt, DeviceClass: Debug> Inspectable for NatRoutines<I, DeviceClass, ()> {
    fn record<Inspector: netstack3_base::Inspector>(&self, inspector: &mut Inspector) {
        let Self { ingress, local_ingress, local_egress, egress } = self;

        inspector.record_child("ingress", |inspector| inspector.delegate_inspectable(ingress));
        inspector.record_child("local_ingress", |inspector| {
            inspector.delegate_inspectable(local_ingress)
        });
        inspector
            .record_child("local_egress", |inspector| inspector.delegate_inspectable(local_egress));
        inspector.record_child("egress", |inspector| inspector.delegate_inspectable(egress));
    }
}

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

/// A one-way boolean toggle that can only go from `false` to `true`.
///
/// Once it has been flipped to `true`, it will remain in that state forever.
#[derive(Default)]
pub struct OneWayBoolean(bool);

impl OneWayBoolean {
    /// A [`OneWayBoolean`] that is enabled on creation.
    pub const TRUE: Self = Self(true);

    /// Get the value of the boolean.
    pub fn get(&self) -> bool {
        let Self(inner) = self;
        *inner
    }

    /// Toggle the boolean to `true`.
    ///
    /// This operation is idempotent: even though the [`OneWayBoolean`]'s value will
    /// only ever change from `false` to `true` once, this method can be called any
    /// number of times safely and the value will remain `true`.
    pub fn set(&mut self) {
        let Self(inner) = self;
        *inner = true;
    }
}

/// IP version-specific filtering state.
pub struct State<I: IpExt, A, BT: FilterBindingsTypes> {
    /// Routines used for filtering packets that are installed on hooks.
    pub installed_routines: ValidRoutines<I, BT::DeviceClass>,
    /// Routines that are only executed if jumped to from other routines.
    ///
    /// Jump rules refer to their targets by holding a reference counted pointer
    /// to the inner routine; we hold this index of all uninstalled routines
    /// that have any references in order to report them in inspect data.
    pub(crate) uninstalled_routines: Vec<UninstalledRoutine<I, BT::DeviceClass, ()>>,
    /// Connection tracking state.
    pub conntrack: conntrack::Table<I, NatConfig<I, A>, BT>,
    /// One-way boolean toggle indicating whether any rules have ever been added to
    /// an installed NAT routine. If not, performing NAT can safely be skipped.
    ///
    /// This is useful because if any NAT is being performed, we have to check
    /// whether it's necessary to perform implicit NAT for *all* traffic -- even if
    /// it doesn't match any NAT rules -- to avoid conflicting tracked connections.
    /// If we know that no NAT is being performed at all, this extra work can be
    /// avoided.
    ///
    /// Note that this value will only ever go from false to true; it does not
    /// indicate whether any NAT rules are *currently* installed. This avoids a race
    /// condition where NAT rules are removed but connections are still being NATed
    /// based on those rules, and therefore must be considered when creating new
    /// connection tracking entries.
    pub nat_installed: OneWayBoolean,
}

impl<I: IpExt, A, BC: FilterBindingsContext> State<I, A, BC> {
    /// Create a new State.
    pub fn new<CC: CoreTimerContext<FilterTimerId<I>, BC>>(bindings_ctx: &mut BC) -> Self {
        Self {
            installed_routines: Default::default(),
            uninstalled_routines: Default::default(),
            conntrack: conntrack::Table::new::<CC>(bindings_ctx),
            nat_installed: OneWayBoolean::default(),
        }
    }
}

impl<I: IpExt, A, BT: FilterBindingsTypes> Inspectable for State<I, A, BT> {
    fn record<Inspector: netstack3_base::Inspector>(&self, inspector: &mut Inspector) {
        let Self { installed_routines, uninstalled_routines, conntrack, nat_installed: _ } = self;
        let Routines { ip, nat } = installed_routines.get();

        inspector.record_child("IP", |inspector| inspector.delegate_inspectable(ip));
        inspector.record_child("NAT", |inspector| inspector.delegate_inspectable(nat));
        inspector.record_child("uninstalled", |inspector| {
            inspector.record_usize("routines", uninstalled_routines.len());
            for routine in uninstalled_routines {
                inspector.delegate_inspectable(routine);
            }
        });

        inspector.record_child("conntrack", |inspector| {
            inspector.delegate_inspectable(conntrack);
        });
    }
}

/// A trait for interacting with the pieces of packet metadata that are
/// important for filtering.
pub trait FilterIpMetadata<I: IpExt, A, BT: FilterBindingsTypes> {
    /// Removes the conntrack connection, if it exists.
    fn take_conntrack_connection(
        &mut self,
    ) -> Option<conntrack::Connection<I, NatConfig<I, A>, BT>>;

    /// Puts a new conntrack connection into the metadata struct, returning the
    /// previous value.
    fn replace_conntrack_connection(
        &mut self,
        conn: conntrack::Connection<I, NatConfig<I, A>, BT>,
    ) -> Option<conntrack::Connection<I, NatConfig<I, A>, BT>>;
}
