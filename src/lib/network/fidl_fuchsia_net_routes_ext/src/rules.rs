// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Extensions for route rules FIDL.

use std::fmt::Debug;
use std::ops::RangeInclusive;

use fidl::endpoints::{DiscoverableProtocolMarker, ProtocolMarker, Proxy as _};
use fidl_fuchsia_net_ext::{IntoExt as _, TryIntoExt as _};
use futures::future::Either;
use futures::{Stream, StreamExt as _, TryStreamExt as _};
use net_types::ip::{GenericOverIp, Ip, IpInvariant, Ipv4, Ipv6, Subnet};
use thiserror::Error;
use {
    fidl_fuchsia_net as fnet, fidl_fuchsia_net_interfaces_admin as fnet_interfaces_admin,
    fidl_fuchsia_net_routes as fnet_routes, fidl_fuchsia_net_routes_admin as fnet_routes_admin,
};

use crate::{impl_responder, FidlRouteIpExt, Responder, SliceResponder, WatcherCreationError};

/// Observation extension for the rules part of `fuchsia.net.routes` FIDL API.
pub trait FidlRuleIpExt: Ip {
    /// The "rules watcher" protocol to use for this IP version.
    type RuleWatcherMarker: ProtocolMarker<RequestStream = Self::RuleWatcherRequestStream>;
    /// The "rules watcher" request stream.
    type RuleWatcherRequestStream: fidl::endpoints::RequestStream<Ok: Send, ControlHandle: Send>;
    /// The rule event to be watched.
    type RuleEvent: From<RuleEvent<Self>>
        + TryInto<RuleEvent<Self>, Error = RuleFidlConversionError>
        + Unpin;
    /// The responder to the watch request.
    type RuleWatcherWatchResponder: SliceResponder<Self::RuleEvent>;

    /// Turns a FIDL rule watcher request into the extension type.
    fn into_rule_watcher_request(
        request: fidl::endpoints::Request<Self::RuleWatcherMarker>,
    ) -> RuleWatcherRequest<Self>;
}

impl_responder!(fnet_routes::RuleWatcherV4WatchResponder, &[fnet_routes::RuleEventV4]);
impl_responder!(fnet_routes::RuleWatcherV6WatchResponder, &[fnet_routes::RuleEventV6]);

impl FidlRuleIpExt for Ipv4 {
    type RuleWatcherMarker = fnet_routes::RuleWatcherV4Marker;
    type RuleWatcherRequestStream = fnet_routes::RuleWatcherV4RequestStream;
    type RuleEvent = fnet_routes::RuleEventV4;
    type RuleWatcherWatchResponder = fnet_routes::RuleWatcherV4WatchResponder;

    fn into_rule_watcher_request(
        request: fidl::endpoints::Request<Self::RuleWatcherMarker>,
    ) -> RuleWatcherRequest<Self> {
        RuleWatcherRequest::from(request)
    }
}

impl FidlRuleIpExt for Ipv6 {
    type RuleWatcherMarker = fnet_routes::RuleWatcherV6Marker;
    type RuleWatcherRequestStream = fnet_routes::RuleWatcherV6RequestStream;
    type RuleEvent = fnet_routes::RuleEventV6;
    type RuleWatcherWatchResponder = fnet_routes::RuleWatcherV6WatchResponder;

    fn into_rule_watcher_request(
        request: fidl::endpoints::Request<Self::RuleWatcherMarker>,
    ) -> RuleWatcherRequest<Self> {
        RuleWatcherRequest::from(request)
    }
}

/// The request for the rules watchers.
pub enum RuleWatcherRequest<I: FidlRuleIpExt> {
    /// Hanging-Get style API for observing routing rule changes.
    Watch {
        /// Responder for the events.
        responder: I::RuleWatcherWatchResponder,
    },
}

impl From<fnet_routes::RuleWatcherV4Request> for RuleWatcherRequest<Ipv4> {
    fn from(req: fnet_routes::RuleWatcherV4Request) -> Self {
        match req {
            fnet_routes::RuleWatcherV4Request::Watch { responder } => {
                RuleWatcherRequest::Watch { responder }
            }
        }
    }
}

impl From<fnet_routes::RuleWatcherV6Request> for RuleWatcherRequest<Ipv6> {
    fn from(req: fnet_routes::RuleWatcherV6Request) -> Self {
        match req {
            fnet_routes::RuleWatcherV6Request::Watch { responder } => {
                RuleWatcherRequest::Watch { responder }
            }
        }
    }
}

/// An installed IPv4 routing rule.
#[derive(Debug, Hash, PartialEq, Eq, Clone)]
pub struct InstalledRule<I: Ip> {
    /// Rule sets are ordered by the rule set priority, rule sets are disjoint
    /// and don’t have interleaving rules among them.
    pub priority: RuleSetPriority,
    /// Rules within a rule set are locally ordered, together with the rule set
    /// priority, this defines a global order for all installed rules.
    pub index: RuleIndex,
    /// The selector part of the rule, the rule is a no-op if the selector does
    /// not match the packet.
    pub selector: RuleSelector<I>,
    /// The action part of the rule that describes what to do if the selector
    /// matches the packet.
    pub action: RuleAction,
}

impl TryFrom<fnet_routes::InstalledRuleV4> for InstalledRule<Ipv4> {
    type Error = RuleFidlConversionError;
    fn try_from(
        fnet_routes::InstalledRuleV4 {
        rule_set_priority,
        rule_index,
        selector,
        action,
    }: fnet_routes::InstalledRuleV4,
    ) -> Result<Self, Self::Error> {
        Ok(Self {
            priority: rule_set_priority.into(),
            index: rule_index.into(),
            selector: selector.try_into()?,
            action: action.into(),
        })
    }
}

impl TryFrom<fnet_routes::InstalledRuleV6> for InstalledRule<Ipv6> {
    type Error = RuleFidlConversionError;
    fn try_from(
        fnet_routes::InstalledRuleV6 {
        rule_set_priority,
        rule_index,
        selector,
        action,
    }: fnet_routes::InstalledRuleV6,
    ) -> Result<Self, Self::Error> {
        Ok(Self {
            priority: rule_set_priority.into(),
            index: rule_index.into(),
            selector: selector.try_into()?,
            action: action.into(),
        })
    }
}

impl From<InstalledRule<Ipv4>> for fnet_routes::InstalledRuleV4 {
    fn from(InstalledRule { priority, index, selector, action }: InstalledRule<Ipv4>) -> Self {
        Self {
            rule_set_priority: priority.into(),
            rule_index: index.into(),
            selector: selector.into(),
            action: action.into(),
        }
    }
}

impl From<InstalledRule<Ipv6>> for fnet_routes::InstalledRuleV6 {
    fn from(InstalledRule { priority, index, selector, action }: InstalledRule<Ipv6>) -> Self {
        Self {
            rule_set_priority: priority.into(),
            rule_index: index.into(),
            selector: selector.into(),
            action: action.into(),
        }
    }
}

/// A rules watcher event.
#[derive(Debug, Clone)]
pub enum RuleEvent<I: Ip> {
    /// A rule that already existed when watching started.
    Existing(InstalledRule<I>),
    /// Sentinel value indicating no more `existing` events will be
    /// received.
    Idle,
    /// A rule that was added while watching.
    Added(InstalledRule<I>),
    /// A rule that was removed while watching.
    Removed(InstalledRule<I>),
}

impl TryFrom<fnet_routes::RuleEventV4> for RuleEvent<Ipv4> {
    type Error = RuleFidlConversionError;
    fn try_from(event: fnet_routes::RuleEventV4) -> Result<Self, Self::Error> {
        match event {
            fnet_routes::RuleEventV4::Existing(rule) => Ok(RuleEvent::Existing(rule.try_into()?)),
            fnet_routes::RuleEventV4::Idle(fnet_routes::Empty) => Ok(RuleEvent::Idle),
            fnet_routes::RuleEventV4::Added(rule) => Ok(RuleEvent::Added(rule.try_into()?)),
            fnet_routes::RuleEventV4::Removed(rule) => Ok(RuleEvent::Removed(rule.try_into()?)),
            fnet_routes::RuleEventV4::__SourceBreaking { unknown_ordinal } => {
                Err(RuleFidlConversionError::UnknownOrdinal {
                    name: "RuleEventV4",
                    unknown_ordinal,
                })
            }
        }
    }
}

impl TryFrom<fnet_routes::RuleEventV6> for RuleEvent<Ipv6> {
    type Error = RuleFidlConversionError;
    fn try_from(event: fnet_routes::RuleEventV6) -> Result<Self, Self::Error> {
        match event {
            fnet_routes::RuleEventV6::Existing(rule) => Ok(RuleEvent::Existing(rule.try_into()?)),
            fnet_routes::RuleEventV6::Idle(fnet_routes::Empty) => Ok(RuleEvent::Idle),
            fnet_routes::RuleEventV6::Added(rule) => Ok(RuleEvent::Added(rule.try_into()?)),
            fnet_routes::RuleEventV6::Removed(rule) => Ok(RuleEvent::Removed(rule.try_into()?)),
            fnet_routes::RuleEventV6::__SourceBreaking { unknown_ordinal } => {
                Err(RuleFidlConversionError::UnknownOrdinal {
                    name: "RuleEventV6",
                    unknown_ordinal,
                })
            }
        }
    }
}

impl From<RuleEvent<Ipv4>> for fnet_routes::RuleEventV4 {
    fn from(event: RuleEvent<Ipv4>) -> Self {
        match event {
            RuleEvent::Existing(r) => Self::Existing(r.into()),
            RuleEvent::Idle => Self::Idle(fnet_routes::Empty),
            RuleEvent::Added(r) => Self::Added(r.into()),
            RuleEvent::Removed(r) => Self::Removed(r.into()),
        }
    }
}

impl From<RuleEvent<Ipv6>> for fnet_routes::RuleEventV6 {
    fn from(event: RuleEvent<Ipv6>) -> Self {
        match event {
            RuleEvent::Existing(r) => Self::Existing(r.into()),
            RuleEvent::Idle => Self::Idle(fnet_routes::Empty),
            RuleEvent::Added(r) => Self::Added(r.into()),
            RuleEvent::Removed(r) => Self::Removed(r.into()),
        }
    }
}

/// Admin extension for the rules part of `fuchsia.net.routes.admin` FIDL API.
pub trait FidlRuleAdminIpExt: Ip {
    /// The "rule table" protocol to use for this IP version.
    type RuleTableMarker: DiscoverableProtocolMarker<RequestStream = Self::RuleTableRequestStream>;
    /// The "rule set" protocol to use for this IP Version.
    type RuleSetMarker: ProtocolMarker<RequestStream = Self::RuleSetRequestStream>;
    /// The request stream for the rule table protocol.
    type RuleTableRequestStream: fidl::endpoints::RequestStream<Ok: Send, ControlHandle: Send>;
    /// The request stream for the rule set protocol.
    type RuleSetRequestStream: fidl::endpoints::RequestStream<Ok: Send, ControlHandle: Send>;
    /// The responder for AddRule requests.
    type RuleSetAddRuleResponder: Responder<
        Payload = Result<(), fnet_routes_admin::RuleSetError>,
        ControlHandle = Self::RuleSetControlHandle,
    >;
    /// The responder for RemoveRule requests.
    type RuleSetRemoveRuleResponder: Responder<
        Payload = Result<(), fnet_routes_admin::RuleSetError>,
        ControlHandle = Self::RuleSetControlHandle,
    >;
    /// The responder for AuthenticateForInterface requests.
    type RuleSetAuthenticateForInterfaceResponder: Responder<
        Payload = Result<(), fnet_routes_admin::RuleSetError>,
    >;
    /// The responder for AuthenticateForRouteTable requests.
    type RuleSetAuthenticateForRouteTableResponder: Responder<
        Payload = Result<(), fnet_routes_admin::RuleSetError>,
    >;
    /// The control handle for RuleTable protocols.
    type RuleTableControlHandle: fidl::endpoints::ControlHandle + Send + Clone;
    /// The control handle for RuleSet protocols.
    type RuleSetControlHandle: fidl::endpoints::ControlHandle + Send + Clone;

    /// Turns a FIDL rule set request into the extension type.
    fn into_rule_set_request(
        request: fidl::endpoints::Request<Self::RuleSetMarker>,
    ) -> RuleSetRequest<Self>;

    /// Turns a FIDL rule table request into the extension type.
    fn into_rule_table_request(
        request: fidl::endpoints::Request<Self::RuleTableMarker>,
    ) -> RuleTableRequest<Self>;
}

impl FidlRuleAdminIpExt for Ipv4 {
    type RuleTableMarker = fnet_routes_admin::RuleTableV4Marker;
    type RuleSetMarker = fnet_routes_admin::RuleSetV4Marker;
    type RuleTableRequestStream = fnet_routes_admin::RuleTableV4RequestStream;
    type RuleSetRequestStream = fnet_routes_admin::RuleSetV4RequestStream;
    type RuleSetAddRuleResponder = fnet_routes_admin::RuleSetV4AddRuleResponder;
    type RuleSetRemoveRuleResponder = fnet_routes_admin::RuleSetV4RemoveRuleResponder;
    type RuleSetAuthenticateForInterfaceResponder =
        fnet_routes_admin::RuleSetV4AuthenticateForInterfaceResponder;
    type RuleSetAuthenticateForRouteTableResponder =
        fnet_routes_admin::RuleSetV4AuthenticateForRouteTableResponder;
    type RuleTableControlHandle = fnet_routes_admin::RuleTableV4ControlHandle;
    type RuleSetControlHandle = fnet_routes_admin::RuleSetV4ControlHandle;

    fn into_rule_set_request(
        request: fidl::endpoints::Request<Self::RuleSetMarker>,
    ) -> RuleSetRequest<Self> {
        RuleSetRequest::from(request)
    }

    fn into_rule_table_request(
        request: fidl::endpoints::Request<Self::RuleTableMarker>,
    ) -> RuleTableRequest<Self> {
        RuleTableRequest::from(request)
    }
}

impl FidlRuleAdminIpExt for Ipv6 {
    type RuleTableMarker = fnet_routes_admin::RuleTableV6Marker;
    type RuleSetMarker = fnet_routes_admin::RuleSetV6Marker;
    type RuleTableRequestStream = fnet_routes_admin::RuleTableV6RequestStream;
    type RuleSetRequestStream = fnet_routes_admin::RuleSetV6RequestStream;
    type RuleSetAddRuleResponder = fnet_routes_admin::RuleSetV6AddRuleResponder;
    type RuleSetRemoveRuleResponder = fnet_routes_admin::RuleSetV6RemoveRuleResponder;
    type RuleSetAuthenticateForInterfaceResponder =
        fnet_routes_admin::RuleSetV6AuthenticateForInterfaceResponder;
    type RuleSetAuthenticateForRouteTableResponder =
        fnet_routes_admin::RuleSetV6AuthenticateForRouteTableResponder;
    type RuleTableControlHandle = fnet_routes_admin::RuleTableV6ControlHandle;
    type RuleSetControlHandle = fnet_routes_admin::RuleSetV6ControlHandle;

    fn into_rule_set_request(
        request: fidl::endpoints::Request<Self::RuleSetMarker>,
    ) -> RuleSetRequest<Self> {
        RuleSetRequest::from(request)
    }

    fn into_rule_table_request(
        request: fidl::endpoints::Request<Self::RuleTableMarker>,
    ) -> RuleTableRequest<Self> {
        RuleTableRequest::from(request)
    }
}

impl_responder!(
    fnet_routes_admin::RuleSetV4AddRuleResponder,
    Result<(), fnet_routes_admin::RuleSetError>,
);
impl_responder!(
    fnet_routes_admin::RuleSetV4RemoveRuleResponder,
    Result<(), fnet_routes_admin::RuleSetError>,
);
impl_responder!(
    fnet_routes_admin::RuleSetV4AuthenticateForInterfaceResponder,
    Result<(), fnet_routes_admin::RuleSetError>,
);
impl_responder!(
    fnet_routes_admin::RuleSetV4AuthenticateForRouteTableResponder,
    Result<(), fnet_routes_admin::RuleSetError>,
);
impl_responder!(
    fnet_routes_admin::RuleSetV6AddRuleResponder,
    Result<(), fnet_routes_admin::RuleSetError>,
);
impl_responder!(
    fnet_routes_admin::RuleSetV6RemoveRuleResponder,
    Result<(), fnet_routes_admin::RuleSetError>,
);
impl_responder!(
    fnet_routes_admin::RuleSetV6AuthenticateForInterfaceResponder,
    Result<(), fnet_routes_admin::RuleSetError>,
);
impl_responder!(
    fnet_routes_admin::RuleSetV6AuthenticateForRouteTableResponder,
    Result<(), fnet_routes_admin::RuleSetError>,
);

/// Conversion error for rule elements.
#[derive(Debug, Error, Clone, Copy, PartialEq)]
pub enum RuleFidlConversionError {
    /// A required field was unset. The provided string is the human-readable
    /// name of the unset field.
    #[error("BaseSelector is missing from the RuleSelector")]
    BaseSelectorMissing,
    /// Destination Subnet conversion failed.
    #[error("failed to convert `destination` to net_types subnet: {0:?}")]
    DestinationSubnet(net_types::ip::SubnetError),
    /// Unknown union variant.
    #[error("unexpected union variant for {name}, got ordinal = ({unknown_ordinal})")]
    #[allow(missing_docs)]
    UnknownOrdinal { name: &'static str, unknown_ordinal: u64 },
}

/// The priority of the rule set, all rule sets are linearized based on this.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RuleSetPriority(u32);

/// The index of a rule within a provided rule set.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RuleIndex(u32);

impl RuleIndex {
    /// Create a new rule index from a scalar.
    pub const fn new(x: u32) -> Self {
        Self(x)
    }
}

impl From<RuleSetPriority> for u32 {
    fn from(RuleSetPriority(x): RuleSetPriority) -> Self {
        x
    }
}

impl From<u32> for RuleSetPriority {
    fn from(x: u32) -> Self {
        Self(x)
    }
}

impl From<RuleIndex> for u32 {
    fn from(RuleIndex(x): RuleIndex) -> Self {
        x
    }
}

impl From<u32> for RuleIndex {
    fn from(x: u32) -> Self {
        Self(x)
    }
}

/// The selector part of the rule that is used to match packets.
///
/// The default selector is the one that matches every packets, i.e., all the
/// fields are none.
#[derive(Debug, Clone, Default, Hash, PartialEq, Eq)]
pub struct RuleSelector<I: Ip> {
    /// Matches whether the source address of the packet is from the subnet.
    pub from: Option<Subnet<I::Addr>>,
    /// Matches the packet iff the packet was locally generated.
    pub locally_generated: Option<bool>,
    /// Matches the packet iff the socket that was bound to the device using
    /// `SO_BINDTODEVICE`.
    pub bound_device: Option<u64>,
    /// The selector for the MARK_1 domain.
    pub mark_1_selector: Option<MarkSelector>,
    /// The selector for the MARK_2 domain.
    pub mark_2_selector: Option<MarkSelector>,
}

impl TryFrom<fnet_routes::RuleSelectorV4> for RuleSelector<Ipv4> {
    type Error = RuleFidlConversionError;
    fn try_from(
        fnet_routes::RuleSelectorV4 {
            from,
            base,
            __source_breaking: fidl::marker::SourceBreaking,
        }: fnet_routes::RuleSelectorV4,
    ) -> Result<Self, Self::Error> {
        let fnet_routes::BaseSelector {
            locally_generated,
            bound_device,
            mark_1_selector,
            mark_2_selector,
            __source_breaking: fidl::marker::SourceBreaking,
        } = base.ok_or(RuleFidlConversionError::BaseSelectorMissing)?;
        Ok(Self {
            from: from
                .map(|from| from.try_into_ext().map_err(RuleFidlConversionError::DestinationSubnet))
                .transpose()?,
            locally_generated,
            bound_device,
            mark_1_selector: mark_1_selector.map(MarkSelector::try_from).transpose()?,
            mark_2_selector: mark_2_selector.map(MarkSelector::try_from).transpose()?,
        })
    }
}

impl From<RuleSelector<Ipv4>> for fnet_routes::RuleSelectorV4 {
    fn from(
        RuleSelector {
            from,
            locally_generated,
            bound_device,
            mark_1_selector,
            mark_2_selector,
        }: RuleSelector<Ipv4>,
    ) -> Self {
        fnet_routes::RuleSelectorV4 {
            from: from.map(|from| fnet::Ipv4AddressWithPrefix {
                addr: from.network().into_ext(),
                prefix_len: from.prefix(),
            }),
            base: Some(fnet_routes::BaseSelector {
                locally_generated,
                bound_device,
                mark_1_selector: mark_1_selector.map(Into::into),
                mark_2_selector: mark_2_selector.map(Into::into),
                __source_breaking: fidl::marker::SourceBreaking,
            }),
            __source_breaking: fidl::marker::SourceBreaking,
        }
    }
}

impl TryFrom<fnet_routes::RuleSelectorV6> for RuleSelector<Ipv6> {
    type Error = RuleFidlConversionError;
    fn try_from(
        fnet_routes::RuleSelectorV6 {
            from,
            base,
            __source_breaking: fidl::marker::SourceBreaking,
        }: fnet_routes::RuleSelectorV6,
    ) -> Result<Self, Self::Error> {
        let fnet_routes::BaseSelector {
            locally_generated,
            bound_device,
            mark_1_selector,
            mark_2_selector,
            __source_breaking: fidl::marker::SourceBreaking,
        } = base.ok_or(RuleFidlConversionError::BaseSelectorMissing)?;
        Ok(Self {
            from: from
                .map(|from| from.try_into_ext().map_err(RuleFidlConversionError::DestinationSubnet))
                .transpose()?,
            locally_generated,
            bound_device,
            mark_1_selector: mark_1_selector.map(MarkSelector::try_from).transpose()?,
            mark_2_selector: mark_2_selector.map(MarkSelector::try_from).transpose()?,
        })
    }
}

impl From<RuleSelector<Ipv6>> for fnet_routes::RuleSelectorV6 {
    fn from(
        RuleSelector {
            from,
            locally_generated,
            bound_device,
            mark_1_selector,
            mark_2_selector,
        }: RuleSelector<Ipv6>,
    ) -> Self {
        fnet_routes::RuleSelectorV6 {
            from: from.map(|from| fnet::Ipv6AddressWithPrefix {
                addr: from.network().into_ext(),
                prefix_len: from.prefix(),
            }),
            base: Some(fnet_routes::BaseSelector {
                locally_generated,
                bound_device,
                mark_1_selector: mark_1_selector.map(Into::into),
                mark_2_selector: mark_2_selector.map(Into::into),
                __source_breaking: fidl::marker::SourceBreaking,
            }),
            __source_breaking: fidl::marker::SourceBreaking,
        }
    }
}

#[derive(Debug, Clone, Hash, PartialEq, Eq)]
/// A selector to be used against the mark value.
pub enum MarkSelector {
    /// This mark domain does not have a mark.
    Unmarked,
    /// This mark domain has a mark.
    Marked {
        /// Mask to apply before comparing to the range in `between`.
        mask: u32,
        /// The mark is between the given range.
        between: RangeInclusive<u32>,
    },
}

impl TryFrom<fnet_routes::MarkSelector> for MarkSelector {
    type Error = RuleFidlConversionError;

    fn try_from(sel: fnet_routes::MarkSelector) -> Result<Self, Self::Error> {
        match sel {
            fnet_routes::MarkSelector::Unmarked(fnet_routes::Unmarked) => {
                Ok(MarkSelector::Unmarked)
            }
            fnet_routes::MarkSelector::Marked(fnet_routes::Marked {
                mask,
                between: fnet_routes::Between { start, end },
            }) => Ok(MarkSelector::Marked { mask, between: RangeInclusive::new(start, end) }),
            fnet_routes::MarkSelector::__SourceBreaking { unknown_ordinal } => {
                Err(RuleFidlConversionError::UnknownOrdinal {
                    name: "MarkSelector",
                    unknown_ordinal,
                })
            }
        }
    }
}

impl From<MarkSelector> for fnet_routes::MarkSelector {
    fn from(sel: MarkSelector) -> Self {
        match sel {
            MarkSelector::Unmarked => fnet_routes::MarkSelector::Unmarked(fnet_routes::Unmarked),
            MarkSelector::Marked { mask, between } => {
                let (start, end) = between.into_inner();
                fnet_routes::MarkSelector::Marked(fnet_routes::Marked {
                    mask,
                    between: fnet_routes::Between { start, end },
                })
            }
        }
    }
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
/// Actions of a rule if the selector matches.
pub enum RuleAction {
    /// Return network is unreachable.
    Unreachable,
    /// Look for a route in the indicated route table. If there is no matching
    /// route in the target table, the lookup will continue to consider the
    /// next rule.
    Lookup(u32),
}

impl From<fnet_routes::RuleAction> for RuleAction {
    fn from(action: fnet_routes::RuleAction) -> Self {
        match action {
            fnet_routes::RuleAction::Lookup(table_id) => RuleAction::Lookup(table_id),
            fnet_routes::RuleAction::Unreachable(fnet_routes::Unreachable) => {
                RuleAction::Unreachable
            }
            fnet_routes::RuleAction::__SourceBreaking { unknown_ordinal } => {
                panic!("unexpected mark selector variant, unknown ordinal: {unknown_ordinal}")
            }
        }
    }
}

impl From<RuleAction> for fnet_routes::RuleAction {
    fn from(action: RuleAction) -> Self {
        match action {
            RuleAction::Unreachable => {
                fnet_routes::RuleAction::Unreachable(fnet_routes::Unreachable)
            }
            RuleAction::Lookup(table_id) => fnet_routes::RuleAction::Lookup(table_id),
        }
    }
}

/// GenericOverIp version of RouteTableV{4, 6}Request.
#[derive(GenericOverIp, Debug)]
#[generic_over_ip(I, Ip)]
pub enum RuleTableRequest<I: FidlRuleAdminIpExt> {
    /// Creates a new rule set for the global rule table.
    NewRuleSet {
        /// The priority of the the rule set.
        priority: RuleSetPriority,
        /// The server end of the rule set protocol.
        rule_set: fidl::endpoints::ServerEnd<I::RuleSetMarker>,
        /// Control handle to the protocol.
        control_handle: I::RuleTableControlHandle,
    },
}

impl From<fnet_routes_admin::RuleTableV4Request> for RuleTableRequest<Ipv4> {
    fn from(value: fnet_routes_admin::RuleTableV4Request) -> Self {
        match value {
            fnet_routes_admin::RuleTableV4Request::NewRuleSet {
                priority,
                rule_set,
                control_handle,
            } => Self::NewRuleSet { priority: RuleSetPriority(priority), rule_set, control_handle },
        }
    }
}

impl From<fnet_routes_admin::RuleTableV6Request> for RuleTableRequest<Ipv6> {
    fn from(value: fnet_routes_admin::RuleTableV6Request) -> Self {
        match value {
            fnet_routes_admin::RuleTableV6Request::NewRuleSet {
                priority,
                rule_set,
                control_handle,
            } => Self::NewRuleSet { priority: RuleSetPriority(priority), rule_set, control_handle },
        }
    }
}

/// GenericOverIp version of RuleSetV{4, 6}Request.
#[derive(GenericOverIp, Debug)]
#[generic_over_ip(I, Ip)]
pub enum RuleSetRequest<I: FidlRuleAdminIpExt> {
    /// Adds a rule to the rule set.
    AddRule {
        /// The index of the rule to be added.
        index: RuleIndex,
        /// The selector of the rule.
        selector: Result<RuleSelector<I>, RuleFidlConversionError>,
        /// The action of the rule.
        action: RuleAction,
        /// The responder for this request.
        responder: I::RuleSetAddRuleResponder,
    },
    /// Removes a rule from the rule set.
    RemoveRule {
        /// The index of the rule to be removed.
        index: RuleIndex,
        /// The responder for this request.
        responder: I::RuleSetRemoveRuleResponder,
    },
    /// Authenticates the rule set for managing routes on an interface.
    AuthenticateForInterface {
        /// The credential proving authorization for this interface.
        credential: fnet_interfaces_admin::ProofOfInterfaceAuthorization,
        /// The responder for this request.
        responder: I::RuleSetAuthenticateForInterfaceResponder,
    },
    /// Authenticates the rule set for managing routes on a route table.
    AuthenticateForRouteTable {
        /// The table id of the table being authenticated for.
        table: u32,
        /// The credential proving authorization for this route table.
        token: fidl::Event,
        /// The responder for this request.
        responder: I::RuleSetAuthenticateForRouteTableResponder,
    },
    /// Closes the rule set
    Close {
        /// The control handle to rule set protocol.
        control_handle: I::RuleSetControlHandle,
    },
}

impl From<fnet_routes_admin::RuleSetV4Request> for RuleSetRequest<Ipv4> {
    fn from(value: fnet_routes_admin::RuleSetV4Request) -> Self {
        match value {
            fnet_routes_admin::RuleSetV4Request::AddRule { index, selector, action, responder } => {
                RuleSetRequest::AddRule {
                    index: RuleIndex(index),
                    selector: selector.try_into(),
                    action: action.into(),
                    responder,
                }
            }
            fnet_routes_admin::RuleSetV4Request::RemoveRule { index, responder } => {
                RuleSetRequest::RemoveRule { index: RuleIndex(index), responder }
            }
            fnet_routes_admin::RuleSetV4Request::AuthenticateForInterface {
                credential,
                responder,
            } => RuleSetRequest::AuthenticateForInterface { credential, responder },
            fnet_routes_admin::RuleSetV4Request::AuthenticateForRouteTable {
                table,
                token,
                responder,
            } => RuleSetRequest::AuthenticateForRouteTable { table, token, responder },
            fnet_routes_admin::RuleSetV4Request::Close { control_handle } => {
                RuleSetRequest::Close { control_handle }
            }
        }
    }
}
impl From<fnet_routes_admin::RuleSetV6Request> for RuleSetRequest<Ipv6> {
    fn from(value: fnet_routes_admin::RuleSetV6Request) -> Self {
        match value {
            fnet_routes_admin::RuleSetV6Request::AddRule { index, selector, action, responder } => {
                RuleSetRequest::AddRule {
                    index: RuleIndex(index),
                    selector: selector.try_into(),
                    action: action.into(),
                    responder,
                }
            }
            fnet_routes_admin::RuleSetV6Request::RemoveRule { index, responder } => {
                RuleSetRequest::RemoveRule { index: RuleIndex(index), responder }
            }
            fnet_routes_admin::RuleSetV6Request::AuthenticateForInterface {
                credential,
                responder,
            } => RuleSetRequest::AuthenticateForInterface { credential, responder },
            fnet_routes_admin::RuleSetV6Request::AuthenticateForRouteTable {
                table,
                token,
                responder,
            } => RuleSetRequest::AuthenticateForRouteTable { table, token, responder },
            fnet_routes_admin::RuleSetV6Request::Close { control_handle } => {
                RuleSetRequest::Close { control_handle }
            }
        }
    }
}

/// Rule set creation errors.
#[derive(Clone, Debug, Error)]
pub enum RuleSetCreationError {
    /// Proxy creation failed.
    #[error("failed to create proxy: {0}")]
    CreateProxy(fidl::Error),
    /// Rule set creation failed.
    #[error("failed to create route set: {0}")]
    RuleSet(fidl::Error),
}

/// Creates a new rule set for the rule table.
pub fn new_rule_set<I: Ip + FidlRuleAdminIpExt>(
    rule_table_proxy: &<I::RuleTableMarker as ProtocolMarker>::Proxy,
    priority: RuleSetPriority,
) -> Result<<I::RuleSetMarker as ProtocolMarker>::Proxy, RuleSetCreationError> {
    let (rule_set_proxy, rule_set_server_end) = fidl::endpoints::create_proxy::<I::RuleSetMarker>()
        .map_err(RuleSetCreationError::CreateProxy)?;

    #[derive(GenericOverIp)]
    #[generic_over_ip(I, Ip)]
    struct NewRuleSetInput<'a, I: FidlRuleAdminIpExt> {
        rule_set_server_end: fidl::endpoints::ServerEnd<I::RuleSetMarker>,
        rule_table_proxy: &'a <I::RuleTableMarker as ProtocolMarker>::Proxy,
    }
    let IpInvariant(result) = I::map_ip::<NewRuleSetInput<'_, I>, _>(
        NewRuleSetInput::<'_, I> { rule_set_server_end, rule_table_proxy },
        |NewRuleSetInput { rule_set_server_end, rule_table_proxy }| {
            IpInvariant(rule_table_proxy.new_rule_set(priority.into(), rule_set_server_end))
        },
        |NewRuleSetInput { rule_set_server_end, rule_table_proxy }| {
            IpInvariant(rule_table_proxy.new_rule_set(priority.into(), rule_set_server_end))
        },
    );

    result.map_err(RuleSetCreationError::RuleSet)?;
    Ok(rule_set_proxy)
}

/// Dispatches `add_rule` on either the `RuleSetV4` or `RuleSetV6` proxy.
pub async fn add_rule<I: Ip + FidlRuleAdminIpExt>(
    rule_set: &<I::RuleSetMarker as ProtocolMarker>::Proxy,
    index: RuleIndex,
    selector: RuleSelector<I>,
    action: RuleAction,
) -> Result<Result<(), fnet_routes_admin::RuleSetError>, fidl::Error> {
    #[derive(GenericOverIp)]
    #[generic_over_ip(I, Ip)]
    struct AddRuleInput<'a, I: FidlRuleAdminIpExt> {
        rule_set: &'a <I::RuleSetMarker as ProtocolMarker>::Proxy,
        index: RuleIndex,
        selector: RuleSelector<I>,
        action: RuleAction,
    }

    let IpInvariant(result_fut) = I::map_ip(
        AddRuleInput { rule_set, index, selector, action },
        |AddRuleInput { rule_set, index, selector, action }| {
            IpInvariant(Either::Left(rule_set.add_rule(
                index.into(),
                &selector.into(),
                &action.into(),
            )))
        },
        |AddRuleInput { rule_set, index, selector, action }| {
            IpInvariant(Either::Right(rule_set.add_rule(
                index.into(),
                &selector.into(),
                &action.into(),
            )))
        },
    );
    result_fut.await
}

/// Dispatches `remove_rule` on either the `RuleSetV4` or `RuleSetV6` proxy.
pub async fn remove_rule<I: Ip + FidlRuleAdminIpExt>(
    rule_set: &<I::RuleSetMarker as ProtocolMarker>::Proxy,
    index: RuleIndex,
) -> Result<Result<(), fnet_routes_admin::RuleSetError>, fidl::Error> {
    #[derive(GenericOverIp)]
    #[generic_over_ip(I, Ip)]
    struct RemoveRuleInput<'a, I: FidlRuleAdminIpExt> {
        rule_set: &'a <I::RuleSetMarker as ProtocolMarker>::Proxy,
        index: RuleIndex,
    }

    let IpInvariant(result_fut) = I::map_ip(
        RemoveRuleInput { rule_set, index },
        |RemoveRuleInput { rule_set, index }| {
            IpInvariant(Either::Left(rule_set.remove_rule(index.into())))
        },
        |RemoveRuleInput { rule_set, index }| {
            IpInvariant(Either::Right(rule_set.remove_rule(index.into())))
        },
    );
    result_fut.await
}

/// Dispatches `close` on either the `RuleSetV4` or `RuleSetV6` proxy.
///
/// Waits until the channel is closed before returning.
pub async fn close_rule_set<I: Ip + FidlRuleAdminIpExt>(
    rule_set: <I::RuleSetMarker as ProtocolMarker>::Proxy,
) -> Result<(), fidl::Error> {
    #[derive(GenericOverIp)]
    #[generic_over_ip(I, Ip)]
    struct CloseInput<'a, I: FidlRuleAdminIpExt> {
        rule_set: &'a <I::RuleSetMarker as ProtocolMarker>::Proxy,
    }

    let IpInvariant(result) = I::map_ip(
        CloseInput { rule_set: &rule_set },
        |CloseInput { rule_set }| IpInvariant(rule_set.close()),
        |CloseInput { rule_set }| IpInvariant(rule_set.close()),
    );

    assert!(rule_set
        .on_closed()
        .await
        .expect("failed to wait for signals")
        .contains(fidl::Signals::CHANNEL_PEER_CLOSED));

    result
}

/// Dispatches either `GetRuleWatcherV4` or `GetRuleWatcherV6` on the state proxy.
pub fn get_rule_watcher<I: FidlRuleIpExt + FidlRouteIpExt>(
    state_proxy: &<I::StateMarker as fidl::endpoints::ProtocolMarker>::Proxy,
) -> Result<<I::RuleWatcherMarker as fidl::endpoints::ProtocolMarker>::Proxy, WatcherCreationError>
{
    let (watcher_proxy, watcher_server_end) =
        fidl::endpoints::create_proxy::<I::RuleWatcherMarker>()
            .map_err(WatcherCreationError::CreateProxy)?;

    #[derive(GenericOverIp)]
    #[generic_over_ip(I, Ip)]
    struct GetWatcherInputs<'a, I: FidlRuleIpExt + FidlRouteIpExt> {
        watcher_server_end: fidl::endpoints::ServerEnd<I::RuleWatcherMarker>,
        state_proxy: &'a <I::StateMarker as fidl::endpoints::ProtocolMarker>::Proxy,
    }
    let result = I::map_ip_in(
        GetWatcherInputs::<'_, I> { watcher_server_end, state_proxy },
        |GetWatcherInputs { watcher_server_end, state_proxy }| {
            state_proxy.get_rule_watcher_v4(
                watcher_server_end,
                &fnet_routes::RuleWatcherOptionsV4::default(),
            )
        },
        |GetWatcherInputs { watcher_server_end, state_proxy }| {
            state_proxy.get_rule_watcher_v6(
                watcher_server_end,
                &fnet_routes::RuleWatcherOptionsV6::default(),
            )
        },
    );

    result.map_err(WatcherCreationError::GetWatcher)?;
    Ok(watcher_proxy)
}

/// Calls `Watch()` on the provided `RuleWatcherV4` or `RuleWatcherV6` proxy.
pub async fn watch<'a, I: FidlRuleIpExt>(
    watcher_proxy: &'a <I::RuleWatcherMarker as fidl::endpoints::ProtocolMarker>::Proxy,
) -> Result<Vec<I::RuleEvent>, fidl::Error> {
    #[derive(GenericOverIp)]
    #[generic_over_ip(I, Ip)]
    struct WatchInputs<'a, I: FidlRuleIpExt> {
        watcher_proxy: &'a <I::RuleWatcherMarker as fidl::endpoints::ProtocolMarker>::Proxy,
    }
    #[derive(GenericOverIp)]
    #[generic_over_ip(I, Ip)]
    struct WatchOutputs<I: FidlRuleIpExt> {
        watch_fut: fidl::client::QueryResponseFut<Vec<I::RuleEvent>>,
    }
    let WatchOutputs { watch_fut } = net_types::map_ip_twice!(
        I,
        WatchInputs { watcher_proxy },
        |WatchInputs { watcher_proxy }| { WatchOutputs { watch_fut: watcher_proxy.watch() } }
    );
    watch_fut.await
}

/// Route watcher `Watch` errors.
#[derive(Clone, Debug, Error)]
pub enum RuleWatchError {
    /// The call to `Watch` returned a FIDL error.
    #[error("the call to `Watch()` failed: {0}")]
    Fidl(fidl::Error),
    /// The event returned by `Watch` encountered a conversion error.
    #[error("failed to convert event returned by `Watch()`: {0}")]
    Conversion(RuleFidlConversionError),
    /// The server returned an empty batch of events.
    #[error("the call to `Watch()` returned an empty batch of events")]
    EmptyEventBatch,
}

/// Creates a rules event stream from the state proxy.
pub fn rule_event_stream_from_state<I: FidlRuleIpExt + FidlRouteIpExt>(
    state: &<I::StateMarker as fidl::endpoints::ProtocolMarker>::Proxy,
) -> Result<impl Stream<Item = Result<RuleEvent<I>, RuleWatchError>>, WatcherCreationError> {
    let watcher = get_rule_watcher::<I>(state)?;
    rule_event_stream_from_watcher(watcher)
}

/// Turns the provided watcher client into a [`RuleEvent`] stream by applying
/// Hanging-Get watch.
///
/// Each call to `Watch` returns a batch of events, which are flattened into a
/// single stream. If an error is encountered while calling `Watch` or while
/// converting the event, the stream is immediately terminated.
pub fn rule_event_stream_from_watcher<I: FidlRuleIpExt>(
    watcher: <I::RuleWatcherMarker as fidl::endpoints::ProtocolMarker>::Proxy,
) -> Result<impl Stream<Item = Result<RuleEvent<I>, RuleWatchError>>, WatcherCreationError> {
    Ok(futures::stream::try_unfold(watcher, |watcher| async {
        let events_batch = watch::<I>(&watcher).await.map_err(RuleWatchError::Fidl)?;
        if events_batch.is_empty() {
            return Err(RuleWatchError::EmptyEventBatch);
        }
        // Convert the `I::RuleEvent` into an `RuleEvent<I>` and return any
        // error.
        let events_batch = events_batch
            .into_iter()
            .map(|event| event.try_into())
            .collect::<Result<Vec<_>, _>>()
            .map_err(RuleWatchError::Conversion)?;
        // Below, `try_flatten` requires that the inner stream yields `Result`s.
        let event_stream = futures::stream::iter(events_batch).map(Ok);
        Ok(Some((event_stream, watcher)))
    })
    // Flatten the stream of event streams into a single event stream.
    .try_flatten())
}

#[cfg(test)]
mod tests {
    use assert_matches::assert_matches;
    use fnet_routes::BaseSelector;

    use super::*;

    #[test]
    fn missing_base_selector_v4() {
        let fidl_selector = fidl_fuchsia_net_routes::RuleSelectorV4 {
            from: None,
            base: None,
            __source_breaking: fidl::marker::SourceBreaking,
        };
        assert_matches!(
            RuleSelector::try_from(fidl_selector),
            Err(RuleFidlConversionError::BaseSelectorMissing)
        );
    }

    #[test]
    fn missing_base_selector_v6() {
        let fidl_selector = fidl_fuchsia_net_routes::RuleSelectorV6 {
            from: None,
            base: None,
            __source_breaking: fidl::marker::SourceBreaking,
        };
        assert_matches!(
            RuleSelector::try_from(fidl_selector),
            Err(RuleFidlConversionError::BaseSelectorMissing)
        );
    }

    #[test]
    fn invalid_destination_subnet_v4() {
        let fidl_selector = fidl_fuchsia_net_routes::RuleSelectorV4 {
            // Invalid, because subnets should not have the "host bits" set.
            from: Some(net_declare::fidl_ip_v4_with_prefix!("192.168.0.1/24")),
            base: Some(BaseSelector::default()),
            __source_breaking: fidl::marker::SourceBreaking,
        };
        assert_matches!(
            RuleSelector::try_from(fidl_selector),
            Err(RuleFidlConversionError::DestinationSubnet(_))
        );
    }

    #[test]
    fn invalid_destination_subnet_v6() {
        let fidl_selector = fidl_fuchsia_net_routes::RuleSelectorV6 {
            // Invalid, because subnets should not have the "host bits" set.
            from: Some(net_declare::fidl_ip_v6_with_prefix!("fe80::1/64")),
            base: Some(BaseSelector::default()),
            __source_breaking: fidl::marker::SourceBreaking,
        };
        assert_matches!(
            RuleSelector::try_from(fidl_selector),
            Err(RuleFidlConversionError::DestinationSubnet(_))
        );
    }

    #[test]
    fn all_unspecified_selector_v4() {
        let fidl_selector = fidl_fuchsia_net_routes::RuleSelectorV4 {
            from: None,
            base: Some(BaseSelector {
                locally_generated: None,
                bound_device: None,
                mark_1_selector: None,
                mark_2_selector: None,
                __source_breaking: fidl::marker::SourceBreaking,
            }),
            __source_breaking: fidl::marker::SourceBreaking,
        };
        assert_matches!(
            RuleSelector::try_from(fidl_selector),
            Ok(RuleSelector {
                from: None,
                locally_generated: None,
                bound_device: None,
                mark_1_selector: None,
                mark_2_selector: None,
            })
        );
    }

    #[test]
    fn all_unspecified_selector_v6() {
        let fidl_selector = fidl_fuchsia_net_routes::RuleSelectorV6 {
            from: None,
            base: Some(BaseSelector {
                locally_generated: None,
                bound_device: None,
                mark_1_selector: None,
                mark_2_selector: None,
                __source_breaking: fidl::marker::SourceBreaking,
            }),
            __source_breaking: fidl::marker::SourceBreaking,
        };
        assert_matches!(
            RuleSelector::try_from(fidl_selector),
            Ok(RuleSelector {
                from: None,
                locally_generated: None,
                bound_device: None,
                mark_1_selector: None,
                mark_2_selector: None,
            })
        );
    }
}
