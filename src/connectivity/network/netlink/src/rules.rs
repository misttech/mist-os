// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! A module for managing policy based routing (PBR) rules.
//! Supports the following NETLINK_ROUTE requests: RTM_GETRULE, RTM_SETRULE, &
//! RTM_DELRULE.

use std::collections::BTreeMap;

use derivative::Derivative;
use either::Either;
use linux_uapi::{
    rt_class_t_RT_TABLE_DEFAULT, rt_class_t_RT_TABLE_LOCAL, rt_class_t_RT_TABLE_MAIN,
};
use net_types::ip::{Ip, IpVersion, Ipv4, Ipv6};
use netlink_packet_core::{NetlinkMessage, NLM_F_MULTIPART};
use netlink_packet_route::rule::{RuleAction, RuleAttribute, RuleMessage};
use netlink_packet_route::{AddressFamily, RouteNetlinkMessage};

use crate::client::InternalClient;
use crate::messaging::Sender;
use crate::netlink_packet::errno::Errno;
use crate::protocol_family::route::NetlinkRoute;
use crate::protocol_family::ProtocolFamily;

// The priorities of the default rules installed on Linux.
const LINUX_DEFAULT_LOOKUP_LOCAL_PRIORITY: u32 = 0;
const LINUX_DEFAULT_LOOKUP_MAIN_PRIORITY: u32 = 32766;
const LINUX_DEFAULT_LOOKUP_DEFAULT_PRIORITY: u32 = 32767;

type RulePriority = u32;

/// Helper to retrieve the `Priority` NLA from a [`RuleMessage`].
fn get_priority(RuleMessage { header: _, attributes, .. }: &RuleMessage) -> Option<RulePriority> {
    attributes.iter().find_map(|nla| match nla {
        RuleAttribute::Priority(priority) => Some(*priority),
        _ => None,
    })
}

/// Returns true if the two rules are equal, ignoring nla order.
fn rules_are_equal(
    RuleMessage { header: header1, attributes: nlas1, .. }: &RuleMessage,
    RuleMessage { header: header2, attributes: nlas2, .. }: &RuleMessage,
) -> bool {
    if header1 != header2 || nlas1.len() != nlas2.len() {
        return false;
    }
    nlas1.iter().all(|nla| nlas2.contains(nla))
}

/// Returns true if the specified pattern is valid.
fn is_valid_del_pattern(RuleMessage { header, attributes, .. }: &RuleMessage) -> bool {
    // Either an action, or an NLA must be specified.
    attributes.len() != 0 || header.action != RuleAction::Unspec
}

/// Returns true if the given rule matches the given deletion pattern.
fn rule_matches_del_pattern(rule: &RuleMessage, del_pattern: &RuleMessage) -> bool {
    let RuleMessage { header: rule_header, attributes: rule_nlas, .. } = rule;
    let RuleMessage { header: pattern_header, attributes: pattern_nlas, .. } = del_pattern;

    // If the pattern specifies an action, it must match the rule's action.
    if pattern_header.action != RuleAction::Unspec && rule_header.action != pattern_header.action {
        return false;
    }

    // Any NLA specified by the pattern must be present in the rule, with the
    // same value.
    for pattern_nla in pattern_nlas {
        if !rule_nlas.iter().any(|rule_nla| rule_nla == pattern_nla) {
            return false;
        }
    }
    true
}

/// Converts the [`RuleMessage`] into a RtnlMessage::NewRule [`NetlinkMessage`].
fn to_nlm_new_rule(
    rule: RuleMessage,
    sequence_number: u32,
    dump: bool,
) -> NetlinkMessage<RouteNetlinkMessage> {
    let mut msg: NetlinkMessage<RouteNetlinkMessage> = RouteNetlinkMessage::NewRule(rule).into();
    msg.header.sequence_number = sequence_number;
    if dump {
        msg.header.flags = NLM_F_MULTIPART;
    }
    msg.finalize();
    msg
}

/// A table of PBR rules.
///
/// Note that Fuchsia does not support policy based routing, so this
/// implementation merely tracks the state of the "rule table", so that requests
/// are handled consistently (E.g. RTM_GETRULE correctly returns rules that were
/// previously installed via RTM_NEWRULE).
#[derive(Default)]
struct RuleTableInner {
    /// The rules held by this rule table.
    ///
    /// The [`BTreeMap`] ensures that the rules are sorted by their
    /// [`RulePriority`], while the held `Vec` ensures the rules at a given
    /// [`RulePriority`] are held in insertion order (new rules are pushed onto
    /// the back). This gives the rule table a consistent ordering based first
    /// on priority, and then by age.
    rules: BTreeMap<RulePriority, Vec<RuleMessage>>,
}

impl RuleTableInner {
    fn new_with_defaults<I: Ip>() -> RuleTableInner {
        fn build_lookup_rule<I: Ip>(priority: RulePriority, table: u8) -> RuleMessage {
            let mut rule = RuleMessage::default();
            rule.header.family = match I::VERSION {
                IpVersion::V4 => AddressFamily::Inet,
                IpVersion::V6 => AddressFamily::Inet6,
            };
            rule.header.table = table;
            rule.header.action = RuleAction::ToTable;
            rule.attributes.push(RuleAttribute::Priority(priority));
            rule
        }

        let mut table = RuleTableInner::default();
        let rules_to_add = match I::VERSION {
            IpVersion::V4 => itertools::Either::Left(
                [
                    build_lookup_rule::<Ipv4>(
                        LINUX_DEFAULT_LOOKUP_LOCAL_PRIORITY,
                        rt_class_t_RT_TABLE_LOCAL as u8,
                    ),
                    build_lookup_rule::<Ipv4>(
                        LINUX_DEFAULT_LOOKUP_MAIN_PRIORITY,
                        rt_class_t_RT_TABLE_MAIN as u8,
                    ),
                    build_lookup_rule::<Ipv4>(
                        LINUX_DEFAULT_LOOKUP_DEFAULT_PRIORITY,
                        rt_class_t_RT_TABLE_DEFAULT as u8,
                    ),
                ]
                .into_iter(),
            ),
            IpVersion::V6 => itertools::Either::Right(
                [
                    build_lookup_rule::<Ipv6>(
                        LINUX_DEFAULT_LOOKUP_LOCAL_PRIORITY,
                        rt_class_t_RT_TABLE_LOCAL as u8,
                    ),
                    build_lookup_rule::<Ipv6>(
                        LINUX_DEFAULT_LOOKUP_MAIN_PRIORITY,
                        rt_class_t_RT_TABLE_MAIN as u8,
                    ),
                ]
                .into_iter(),
            ),
        };

        for rule in rules_to_add {
            table.add_rule(rule).expect("should not fail to add a default rule");
        }

        table
    }

    /// Adds the given rule to the table.
    fn add_rule(&mut self, mut rule: RuleMessage) -> Result<(), AddRuleError> {
        // Get the rule's priority, setting it to a default if unset.
        let priority = if let Some(priority) = get_priority(&rule) {
            priority
        } else {
            let priority = self.default_priority();
            rule.attributes.push(RuleAttribute::Priority(priority));
            priority
        };

        let rules_at_priority = self.rules.entry(priority).or_default();
        if rules_at_priority.iter().any(|existing_rule| rules_are_equal(existing_rule, &rule)) {
            return Err(AddRuleError::AlreadyExists);
        }
        rules_at_priority.push(rule);
        Ok(())
    }

    /// Deletes the first rule from the table that matches the given pattern.
    fn del_rule(&mut self, del_pattern: &RuleMessage) -> Result<(), DelRuleError> {
        if !is_valid_del_pattern(del_pattern) {
            return Err(DelRuleError::InvalidPattern);
        }

        struct RuleKey {
            /// The rule's priority (e.g. the key into the [`BTreeMap`]).
            priority: RulePriority,
            /// The index of the rule in the [`Vec`] at the given priority.
            index: usize,
        }

        // Construct an iterator of (RuleKey, Rule).
        let candidate_rules = {
            let rules_by_priority = if let Some(priority) = get_priority(del_pattern) {
                // If the deletion pattern specifies a priority, reduce the set
                // of candidate rules to only those with the priority
                Either::Left(self.rules.get(&priority).map(|v| (priority, v)).into_iter())
            } else {
                // Otherwise, search the entire table.
                Either::Right(self.rules.iter().map(|(priority, v)| (*priority, v)))
            };
            rules_by_priority
                .into_iter()
                .map(|(priority, v)| {
                    v.iter()
                        .enumerate()
                        .map(move |(index, rule)| (RuleKey { priority, index }, rule))
                })
                .flatten()
        };

        // Select the first suitable rule for deletion.
        let matching_rule = candidate_rules
            .into_iter()
            .find(|(_key, rule)| rule_matches_del_pattern(rule, del_pattern));
        let Some((RuleKey { priority, index }, _rule)) = matching_rule else {
            return Err(DelRuleError::NoMatchesForPattern);
        };

        let no_more_rules_at_priority = {
            let rules_at_priority = self
                .rules
                .get_mut(&priority)
                .expect("RuleTable must have entry for existing priority");
            let _: RuleMessage = rules_at_priority.remove(index);
            rules_at_priority.is_empty()
        };
        // Clear out the empty entry from the btreemap.
        if no_more_rules_at_priority {
            assert_eq!(self.rules.remove(&priority), Some(Vec::new()));
        }
        Ok(())
    }

    /// Iterate over all the rules.
    ///
    /// The rules are ordered first by [`RulePriority`], and second by age.
    fn iter_rules(&self) -> impl Iterator<Item = &RuleMessage> {
        self.rules.values().flatten()
    }

    /// Returns the default_priority to use for a newly installed rule.
    ///
    /// For conformance with Linux, new rules should have their priority set to
    /// `n-1` where n is the priority of the second rule in the table.
    fn default_priority(&self) -> RulePriority {
        if let Some(second_rule) = self.iter_rules().skip(1).next() {
            get_priority(second_rule)
                .expect("rules installed in the RuleTable must have a priority")
                .saturating_sub(1)
        } else {
            0
        }
    }
}

/// Possible errors when adding a rule to a [`RuleTable`].
#[derive(Debug)]
enum AddRuleError {
    AlreadyExists,
}

impl AddRuleError {
    fn errno(&self) -> Errno {
        match self {
            AddRuleError::AlreadyExists => Errno::EEXIST,
        }
    }
}

/// Possible errors when deleting a rule from a [`RuleTable`].
#[derive(Debug)]
enum DelRuleError {
    NoMatchesForPattern,
    InvalidPattern,
}

impl DelRuleError {
    fn errno(&self) -> Errno {
        match self {
            DelRuleError::NoMatchesForPattern => Errno::ENOENT,
            DelRuleError::InvalidPattern => Errno::ENOTSUP,
        }
    }
}
/// Holds an IPv4 and an IPv6 [`RuleTableInner`].
///
/// The inner rule tables are wrapped with `Arc<Mutex>` to support concurrent
/// access from multiple NETLINK_ROUTE clients.
pub(crate) struct RuleTable {
    v4_rules: RuleTableInner,
    v6_rules: RuleTableInner,
}

impl RuleTable {
    #[cfg(test)]
    /// Constructs an empty RuleTable.
    pub(crate) fn new() -> RuleTable {
        RuleTable { v4_rules: RuleTableInner::default(), v6_rules: RuleTableInner::default() }
    }

    /// Constructs a RuleTable prepopulated with the default rules present on
    /// Linux.
    /// * [V4] 0:        from all lookup local
    /// * [V4] 32766:    from all lookup main
    /// * [V4] 32767:    from all lookup default
    /// * [V6] 0:        from all lookup local
    /// * [V6] 32766:    from all lookup main
    pub(crate) fn new_with_defaults() -> RuleTable {
        RuleTable {
            v4_rules: RuleTableInner::new_with_defaults::<Ipv4>(),
            v6_rules: RuleTableInner::new_with_defaults::<Ipv6>(),
        }
    }
}

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
#[derive(Derivative)]
#[derivative(Debug(bound = ""))]
pub(crate) struct RuleRequest<S: Sender<<NetlinkRoute as ProtocolFamily>::InnerMessage>> {
    /// The arguments for this request.
    pub(crate) args: RuleRequestArgs,
    /// The IP Version of this request.
    pub(crate) ip_version: IpVersion,
    /// The request's sequence number.
    pub(crate) sequence_number: u32,
    /// The client that made the request.
    pub(crate) client: InternalClient<NetlinkRoute, S>,
}

/// Handler trait for NETLINK_ROUTE requests related to PBR rules.
pub(crate) trait RuleRequestHandler<S: Sender<<NetlinkRoute as ProtocolFamily>::InnerMessage>>:
    Send + 'static
{
    fn handle_request(&mut self, req: RuleRequest<S>) -> Result<(), Errno>;
}

impl<S: Sender<<NetlinkRoute as ProtocolFamily>::InnerMessage>> RuleRequestHandler<S>
    for RuleTable
{
    fn handle_request(&mut self, req: RuleRequest<S>) -> Result<(), Errno> {
        let RuleTable { v4_rules, v6_rules } = self;
        let RuleRequest { args, ip_version, sequence_number, mut client } = req;
        let rule_table = match ip_version {
            IpVersion::V4 => v4_rules,
            IpVersion::V6 => v6_rules,
        };

        match args {
            RuleRequestArgs::DumpRules => {
                for rule in rule_table.iter_rules() {
                    client.send_unicast(to_nlm_new_rule(rule.clone(), sequence_number, true));
                }
                Ok(())
            }
            RuleRequestArgs::New(rule) => {
                rule_table.add_rule(rule).map_err(|e| e.errno())
                // TODO(https://issues.fuchsia.dev/292587350): Notify
                // multicast groups of `RTM_NEWRULE`.
            }
            RuleRequestArgs::Del(del_pattern) => {
                rule_table.del_rule(&del_pattern).map_err(|e| e.errno())
                // TODO(https://issues.fuchsia.dev/292587350): Notify
                // multicast groups of `RTM_DELRULE`.
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use linux_uapi::{AF_INET, AF_INET6, FR_ACT_TO_TBL, FR_ACT_UNSPEC};
    use test_case::test_case;

    use crate::messaging::testutil::{FakeSender, FakeSenderSink, SentMessage};

    const DUMP_SEQUENCE_NUM: u32 = 999;

    fn build_rule(action: u32, nlas: Vec<RuleAttribute>) -> RuleMessage {
        // This conversion is safe because action is actually a u8,
        // but out binding generator incorrectly emits a u32.
        let action = action as u8;
        let mut rule = RuleMessage::default();
        rule.header.action = RuleAction::from(action);
        rule.attributes = nlas;
        rule
    }

    /// Helper function to dump the rules in the rule table.
    fn dump_rules(
        sink: &mut FakeSenderSink<RouteNetlinkMessage>,
        client: InternalClient<NetlinkRoute, FakeSender<RouteNetlinkMessage>>,
        table: &mut RuleTable,
        ip_version: IpVersion,
    ) -> Vec<NetlinkMessage<RouteNetlinkMessage>> {
        table
            .handle_request(RuleRequest {
                args: RuleRequestArgs::DumpRules,
                ip_version,
                sequence_number: DUMP_SEQUENCE_NUM,
                client: client,
            })
            .expect("dump rules should succeed");
        sink.take_messages().into_iter().map(|SentMessage { message, group: _ }| message).collect()
    }

    #[test_case(None)]
    #[test_case(Some(1))]
    fn test_get_priority(priority: Option<RulePriority>) {
        let mut rule = RuleMessage::default();
        if let Some(priority) = priority {
            rule.attributes.push(RuleAttribute::Priority(priority));
        }
        assert_eq!(get_priority(&rule), priority);
    }
    // Casts to u8 are safe as these are constants which fit into a u8.
    #[test_case(
        build_rule(FR_ACT_UNSPEC, vec![]),
        build_rule(FR_ACT_TO_TBL, vec![]),
        false; "different headers"
    )]
    #[test_case(
        build_rule(FR_ACT_TO_TBL, vec![RuleAttribute::Priority(1)]),
        build_rule(FR_ACT_TO_TBL, vec![]),
        false; "different nlas"
    )]
    #[test_case(
        build_rule(FR_ACT_TO_TBL, vec![RuleAttribute::Priority(1)]),
        build_rule(FR_ACT_TO_TBL, vec![RuleAttribute::Priority(1)]),
        true; "same header and nlas"
    )]
    #[test_case(
        build_rule(FR_ACT_TO_TBL, vec![RuleAttribute::Oifname(String::from("lo")), RuleAttribute::Priority(1)]),
        build_rule(FR_ACT_TO_TBL, vec![RuleAttribute::Priority(1), RuleAttribute::Oifname(String::from("lo"))]),
        true; "different nla order"
    )]
    fn test_rules_are_equal(rule1: RuleMessage, rule2: RuleMessage, equal: bool) {
        assert_eq!(rules_are_equal(&rule1, &rule2), equal);
    }

    #[test_case(FR_ACT_UNSPEC as u8, vec![], false; "no_action_and_no_nlas_is_invalid")]
    #[test_case(FR_ACT_TO_TBL as u8, vec![], true; "action_and_no_nlas_is_valid")]
    #[test_case(FR_ACT_UNSPEC as u8, vec![RuleAttribute::Priority(1)], true; "no_action_and_nlas_is_valid")]
    #[test_case(FR_ACT_UNSPEC as u8, vec![
        RuleAttribute::Priority(1),
        RuleAttribute::Oifname(String::from("lo")),
        ], true; "no_action_and_multiple nlas_is_valid")]
    #[test_case(FR_ACT_TO_TBL as u8, vec![RuleAttribute::Priority(1)], true; "action_and_nlas_is_valid")]
    fn test_is_valid_del_pattern(action: u8, nlas: Vec<RuleAttribute>, expect_valid: bool) {
        let rule = build_rule(action.into(), nlas);
        assert_eq!(is_valid_del_pattern(&rule), expect_valid);
    }

    #[test_case(
        FR_ACT_UNSPEC as u8, FR_ACT_TO_TBL as u8,
        vec![], vec![],
        false; "mismatched_action")]
    #[test_case(
        FR_ACT_TO_TBL as u8, FR_ACT_TO_TBL as u8,
        vec![], vec![RuleAttribute::Priority(1)],
        false; "absent_nla")]
    #[test_case(
        FR_ACT_TO_TBL as u8, FR_ACT_TO_TBL as u8,
        vec![RuleAttribute::Priority(2)], vec![RuleAttribute::Priority(1)],
        false; "mismatched_nla")]
    #[test_case(
        FR_ACT_TO_TBL as u8, FR_ACT_TO_TBL as u8,
        vec![RuleAttribute::Priority(1)], vec![RuleAttribute::Priority(1)],
        true; "exact_match")]
    #[test_case(
        FR_ACT_TO_TBL as u8, FR_ACT_UNSPEC as u8,
        vec![RuleAttribute::Priority(1)], vec![RuleAttribute::Priority(1)],
        true; "more_specific_action_matches")]
    #[test_case(
        FR_ACT_UNSPEC as u8, FR_ACT_UNSPEC as u8,
        vec![RuleAttribute::Priority(1), RuleAttribute::Oifname(String::from("lo"))], vec![RuleAttribute::Priority(1)],
        true; "more_specific_nla_matches")]
    fn test_rule_matches_del_pattern(
        rule_action: u8,
        pattern_action: u8,
        rule_nlas: Vec<RuleAttribute>,
        pattern_nlas: Vec<RuleAttribute>,
        expect_match: bool,
    ) {
        let rule = build_rule(rule_action.into(), rule_nlas);
        let pattern = build_rule(pattern_action.into(), pattern_nlas);
        assert_eq!(rule_matches_del_pattern(&rule, &pattern), expect_match);
    }

    #[test_case(&[], 0; "no_existing_rules_defaults_to_zero")]
    #[test_case(&[99], 0; "one_existing_rules_defaults_to_zero")]
    #[test_case(&[0, 100], 99; "two_existing_rules_defaults_to_second_minus_1")]
    #[test_case(&[0, 100, 200], 99; "three_existing_rules_defaults_to_second_minus_1")]
    #[test_case(&[0, 1], 0; "default_priority_duplicates_existing_priority")]
    #[test_case(&[0, 0], 0; "default_priority_saturates_at_0")]
    fn test_rule_table_default_priority(
        existing_rule_priorities: &[RulePriority],
        expected_default_priority: RulePriority,
    ) {
        let mut table = RuleTableInner::default();
        for (index, priority) in existing_rule_priorities.iter().enumerate() {
            // Give each rule a different `OifName` to avoid "already exists"
            // conflicts.
            let name = RuleAttribute::Oifname(index.to_string());
            table
                .add_rule(build_rule(FR_ACT_UNSPEC, vec![RuleAttribute::Priority(*priority), name]))
                .expect("add rule should succeed");
        }

        assert_eq!(table.default_priority(), expected_default_priority);
    }

    #[test]
    fn test_rule_table_frees_unused_priorities() {
        let mut table = RuleTableInner::default();
        const PRIORITY: RulePriority = 99;
        let rule = build_rule(FR_ACT_UNSPEC, vec![RuleAttribute::Priority(PRIORITY)]);

        table.add_rule(rule.clone()).expect("add rule should succeed");
        assert!(table.rules.contains_key(&PRIORITY));
        // Remove the rule, and verify the priority was removed (as opposed to
        // still existing and holding an empty vec).
        table.del_rule(&rule).expect("del rule should succeed");
        assert!(!table.rules.contains_key(&PRIORITY));
    }

    #[test_case(IpVersion::V4; "v4")]
    #[test_case(IpVersion::V6; "v6")]
    fn test_rule_table(ip_version: IpVersion) {
        let (mut sink, client) =
            crate::client::testutil::new_fake_client(crate::client::testutil::CLIENT_ID_1, &[]);
        let mut table = RuleTable::new();

        // Verify that the table is empty.
        assert_eq!(&dump_rules(&mut sink, client.clone(), &mut table, ip_version)[..], &[],);

        const LOW_PRIORITY: RulePriority = 100;
        const HIGH_PRIORITY: RulePriority = 200;

        // Add a new rule and expect success.
        // Conversion is safe as FR_ACT_TO_TBL (1), fits into a u8.
        let low_priority_rule =
            build_rule(FR_ACT_TO_TBL, vec![RuleAttribute::Priority(LOW_PRIORITY)]);
        table
            .handle_request(RuleRequest {
                args: RuleRequestArgs::New(low_priority_rule.clone()),
                ip_version,
                sequence_number: 0,
                client: client.clone(),
            })
            .expect("new rule should succeed");
        assert_eq!(
            &dump_rules(&mut sink, client.clone(), &mut table, ip_version)[..],
            &[to_nlm_new_rule(low_priority_rule.clone(), DUMP_SEQUENCE_NUM, true)]
        );

        // Adding a "different" rule with the same priority should succeed.
        let newer_low_priority_rule = build_rule(
            FR_ACT_TO_TBL,
            vec![RuleAttribute::Priority(LOW_PRIORITY), RuleAttribute::Oifname(String::from("lo"))],
        );
        table
            .handle_request(RuleRequest {
                args: RuleRequestArgs::New(newer_low_priority_rule.clone()),
                ip_version,
                sequence_number: 0,
                client: client.clone(),
            })
            .expect("new rule should succeed");
        assert_eq!(
            &dump_rules(&mut sink, client.clone(), &mut table, ip_version)[..],
            // Ordered oldest to newest
            &[
                to_nlm_new_rule(low_priority_rule.clone(), DUMP_SEQUENCE_NUM, true),
                to_nlm_new_rule(newer_low_priority_rule.clone(), DUMP_SEQUENCE_NUM, true),
            ]
        );

        // Adding the "same" rule with a different priority should succeed.
        let high_priority_rule =
            build_rule(FR_ACT_TO_TBL, vec![RuleAttribute::Priority(HIGH_PRIORITY)]);
        table
            .handle_request(RuleRequest {
                args: RuleRequestArgs::New(high_priority_rule.clone()),
                ip_version,
                sequence_number: 0,
                client: client.clone(),
            })
            .expect("new rule should succeed");
        assert_eq!(
            &dump_rules(&mut sink, client.clone(), &mut table, ip_version)[..],
            &[
                // Ordered in ascending priority
                to_nlm_new_rule(low_priority_rule.clone(), DUMP_SEQUENCE_NUM, true),
                to_nlm_new_rule(newer_low_priority_rule.clone(), DUMP_SEQUENCE_NUM, true),
                to_nlm_new_rule(high_priority_rule.clone(), DUMP_SEQUENCE_NUM, true),
            ]
        );

        // Specify a deletion pattern that matches all three existing rules, and
        // expect the oldest, lowest priority rule to be removed first.
        let del_pattern_match_all = build_rule(FR_ACT_TO_TBL, vec![]);
        table
            .handle_request(RuleRequest {
                args: RuleRequestArgs::Del(del_pattern_match_all.clone()),
                ip_version,
                sequence_number: 0,
                client: client.clone(),
            })
            .expect("del rule should succeed");
        assert_eq!(
            &dump_rules(&mut sink, client.clone(), &mut table, ip_version)[..],
            &[
                to_nlm_new_rule(newer_low_priority_rule.clone(), DUMP_SEQUENCE_NUM, true),
                to_nlm_new_rule(high_priority_rule.clone(), DUMP_SEQUENCE_NUM, true),
            ]
        );

        // Specify a deletion pattern that only matches the high_priority_rule,
        // and expect it to be deleted.
        // Conversion is safe as FR_ACT_TO_TBL (1) fits into a u8.
        let del_pattern_match_high_priority =
            build_rule(FR_ACT_TO_TBL, vec![RuleAttribute::Priority(HIGH_PRIORITY)]);
        table
            .handle_request(RuleRequest {
                args: RuleRequestArgs::Del(del_pattern_match_high_priority),
                ip_version,
                sequence_number: 0,
                client: client.clone(),
            })
            .expect("del rule should succeed");
        assert_eq!(
            &dump_rules(&mut sink, client.clone(), &mut table, ip_version)[..],
            &[to_nlm_new_rule(newer_low_priority_rule.clone(), DUMP_SEQUENCE_NUM, true)]
        );

        // Delete the final rule.
        // Conversion is safe as FR_ACT_TO_TBL (1) fits into a u8.
        let del_pattern_match_all = build_rule(FR_ACT_TO_TBL, vec![]);
        table
            .handle_request(RuleRequest {
                args: RuleRequestArgs::Del(del_pattern_match_all),
                ip_version,
                sequence_number: 0,
                client: client.clone(),
            })
            .expect("del rule should succeed");
        assert_eq!(&dump_rules(&mut sink, client.clone(), &mut table, ip_version)[..], &[]);
    }

    #[test_case(IpVersion::V4; "v4")]
    #[test_case(IpVersion::V6; "v6")]
    fn test_rule_table_new_rule_already_exists(ip_version: IpVersion) {
        let (mut sink, client) =
            crate::client::testutil::new_fake_client(crate::client::testutil::CLIENT_ID_1, &[]);
        let mut table = RuleTable::new();

        const PRIORITY_NLA: RuleAttribute = RuleAttribute::Priority(0);
        let oif_nla = RuleAttribute::Oifname(String::from("lo"));

        // Add a new rule and expect success.
        // Conversion is safe as FR_ACT_UNSPEC (0) fits into a u8.
        let rule = build_rule(FR_ACT_UNSPEC, vec![oif_nla.clone(), PRIORITY_NLA]);
        table
            .handle_request(RuleRequest {
                args: RuleRequestArgs::New(rule.clone()),
                ip_version,
                sequence_number: 0,
                client: client.clone(),
            })
            .expect("new rule should succeed");
        assert_eq!(
            &dump_rules(&mut sink, client.clone(), &mut table, ip_version)[..],
            &[to_nlm_new_rule(rule.clone(), DUMP_SEQUENCE_NUM, true)]
        );

        // Adding the same rule should return EEXIST.
        let result = table.handle_request(RuleRequest {
            args: RuleRequestArgs::New(rule.clone()),
            ip_version,
            sequence_number: 0,
            client: client.clone(),
        });
        assert_eq!(result, Err(Errno::EEXIST));

        // Adding the same rule with out-of-order NLAs should return EEXIST.
        // Conversion is safe as FR_ACT_UNSPEC (0) fits into a u8.
        let out_of_order_rule = build_rule(FR_ACT_UNSPEC, vec![PRIORITY_NLA, oif_nla.clone()]);
        let result = table.handle_request(RuleRequest {
            args: RuleRequestArgs::New(out_of_order_rule),
            ip_version,
            sequence_number: 0,
            client: client.clone(),
        });
        assert_eq!(result, Err(Errno::EEXIST));

        // Try again, but erase the `Priority` NLA. This confirms EEXIST is
        // still reported when the default priority would conflict with an
        // existing_rule
        // Conversion is safe as FR_ACT_UNSPEC (0) fits into a u8.
        let rule_without_priority = build_rule(FR_ACT_UNSPEC, vec![oif_nla.clone()]);
        let result = table.handle_request(RuleRequest {
            args: RuleRequestArgs::New(rule_without_priority),
            ip_version,
            sequence_number: 0,
            client: client.clone(),
        });
        assert_eq!(result, Err(Errno::EEXIST));
    }

    // Conversions are safe as these constants fit into a u8.
    #[test_case(RuleMessage::default(), Errno::ENOTSUP, IpVersion::V4;
        "empty_patern_not_supported_v4")]
    #[test_case(build_rule(FR_ACT_TO_TBL, vec![]), Errno::ENOENT, IpVersion::V4;
        "no_matching_rules_v4")]
    #[test_case(RuleMessage::default(), Errno::ENOTSUP, IpVersion::V4;
        "empty_patern_not_supported_v6")]
    #[test_case(build_rule(FR_ACT_TO_TBL, vec![]), Errno::ENOENT, IpVersion::V4;
        "no_matching_rules_v6")]
    fn test_rule_table_del_rule_fails(pattern: RuleMessage, error: Errno, ip_version: IpVersion) {
        let (mut sink, client) =
            crate::client::testutil::new_fake_client(crate::client::testutil::CLIENT_ID_1, &[]);
        let mut table = RuleTable::new();
        assert_eq!(&dump_rules(&mut sink, client.clone(), &mut table, ip_version)[..], &[]);

        let result = table.handle_request(RuleRequest {
            args: RuleRequestArgs::Del(pattern),
            ip_version,
            sequence_number: 0,
            client: client,
        });
        assert_eq!(result, Err(error));
    }

    #[test_case(IpVersion::V4, IpVersion::V6; "v4_independent_from_v6")]
    #[test_case(IpVersion::V6, IpVersion::V4; "v6_independent_from_v4")]
    fn test_v4_and_v6_rule_tables_are_independent(version: IpVersion, opposite_version: IpVersion) {
        let (mut sink, client) =
            crate::client::testutil::new_fake_client(crate::client::testutil::CLIENT_ID_1, &[]);
        let mut table = RuleTable::new();
        // Add a new rule to the table and expect success.
        // Conversion is safe as FR_ACT_UNSPEC (0) fits into a u8.
        let rule = build_rule(FR_ACT_UNSPEC, vec![RuleAttribute::Priority(1)]);
        table
            .handle_request(RuleRequest {
                args: RuleRequestArgs::New(rule.clone()),
                ip_version: version,
                sequence_number: 0,
                client: client.clone(),
            })
            .expect("new rule should succeed");
        assert_eq!(
            &dump_rules(&mut sink, client.clone(), &mut table, version)[..],
            &[to_nlm_new_rule(rule.clone(), DUMP_SEQUENCE_NUM, true)]
        );

        // The rule should not be present in the opposite_version's table.
        assert_eq!(&dump_rules(&mut sink, client.clone(), &mut table, opposite_version)[..], &[]);

        // Attempting to delete the rule from the opposite_version's table
        // should fail.
        let result = table.handle_request(RuleRequest {
            args: RuleRequestArgs::Del(rule.clone()),
            ip_version: opposite_version,
            sequence_number: 0,
            client: client.clone(),
        });
        assert_eq!(result, Err(Errno::ENOENT));

        // Attempting to add the same rule to the opposite_version's table
        // should succeed.
        table
            .handle_request(RuleRequest {
                args: RuleRequestArgs::New(rule.clone()),
                ip_version: opposite_version,
                sequence_number: 0,
                client: client.clone(),
            })
            .expect("new rule should succeed");
    }

    #[test]
    fn test_default_rules() {
        let (mut sink, client) =
            crate::client::testutil::new_fake_client(crate::client::testutil::CLIENT_ID_1, &[]);
        let mut table = RuleTable::new_with_defaults();

        let new_rule = |table: u8, priority: RulePriority, family: u16| {
            let mut rule = RuleMessage::default();
            rule.header.action = RuleAction::ToTable;
            rule.header.table = table;
            // Conversion is safe as family is guaranteed to fit into a u8.
            rule.header.family = AddressFamily::from(family as u8);
            rule.attributes = vec![RuleAttribute::Priority(priority)];
            to_nlm_new_rule(rule, DUMP_SEQUENCE_NUM, true)
        };
        // Conversion is safe as these compile-time constants are guaranteed to fit into a u8.
        assert_eq!(
            &dump_rules(&mut sink, client.clone(), &mut table, IpVersion::V4)[..],
            &[
                new_rule(
                    rt_class_t_RT_TABLE_LOCAL as u8,
                    LINUX_DEFAULT_LOOKUP_LOCAL_PRIORITY,
                    AF_INET as u16
                ),
                new_rule(
                    rt_class_t_RT_TABLE_MAIN as u8,
                    LINUX_DEFAULT_LOOKUP_MAIN_PRIORITY,
                    AF_INET as u16
                ),
                new_rule(
                    rt_class_t_RT_TABLE_DEFAULT as u8,
                    LINUX_DEFAULT_LOOKUP_DEFAULT_PRIORITY,
                    AF_INET as u16
                ),
            ]
        );

        assert_eq!(
            &dump_rules(&mut sink, client.clone(), &mut table, IpVersion::V6)[..],
            &[
                new_rule(
                    rt_class_t_RT_TABLE_LOCAL as u8,
                    LINUX_DEFAULT_LOOKUP_LOCAL_PRIORITY,
                    AF_INET6 as u16
                ),
                new_rule(
                    rt_class_t_RT_TABLE_MAIN as u8,
                    LINUX_DEFAULT_LOOKUP_MAIN_PRIORITY,
                    AF_INET6 as u16
                ),
            ]
        );
    }
}
