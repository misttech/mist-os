// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A module for managing policy based routing (PBR) rules.
//! Supports the following NETLINK_ROUTE requests: RTM_GETRULE, RTM_SETRULE, &
//! RTM_DELRULE.

use netlink_packet_route::rtnl::RuleMessage;

use crate::{
    client::InternalClient,
    messaging::Sender,
    netlink_packet::errno::Errno,
    protocol_family::{route::NetlinkRoute, ProtocolFamily},
};

/// A table of PBR rules.
///
/// Note that Fuchsia does not support policy based routing, so this
/// implementation merely tracks the state of the "rule table", so that requests
/// are handled consistently (E.g. RTM_GETRULE correctly returns rules that were
/// previously installed via RTM_NEWRULE).
#[derive(Default, Clone)]
pub(crate) struct RuleTable {}

/// The set of possible requests related to PBR rules.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum RuleRequestArgs {
    /// A RTM_GETRULE request with the NLM_F_DUMP flag set.
    /// Note that non-dump RTM_GETRULE requests are not supported by Netlink
    /// (this is also true on Linux).
    DumpRules,
    // A RTM_NEWRULE request. Holds the rule to be added.
    New(RuleMessage),
    // A RTM_DELRULE request. Holds the rule to be deleted.
    Del(RuleMessage),
}

/// A Netlink request related to PBR rules.
pub(crate) struct RuleRequest<S: Sender<<NetlinkRoute as ProtocolFamily>::InnerMessage>> {
    /// The arguments for this request.
    pub(crate) args: RuleRequestArgs,
    /// The request's sequence number.
    pub(crate) sequence_number: u32,
    /// The client that made the request.
    pub(crate) client: InternalClient<NetlinkRoute, S>,
}

/// Handler trait for NETLINK_ROUTE requests related to PBR rules.
pub(crate) trait RuleRequestHandler<S: Sender<<NetlinkRoute as ProtocolFamily>::InnerMessage>>:
    Clone + Send + 'static
{
    fn handle_request(&mut self, req: RuleRequest<S>) -> Result<(), Errno>;
}

impl<S: Sender<<NetlinkRoute as ProtocolFamily>::InnerMessage>> RuleRequestHandler<S>
    for RuleTable
{
    fn handle_request(&mut self, req: RuleRequest<S>) -> Result<(), Errno> {
        // TODO(https://issuetracker.google.com/283134947): Stub rule requests.
        #[allow(unused_variables)]
        let RuleRequest { args, sequence_number, client } = req;
        Ok(())
    }
}
