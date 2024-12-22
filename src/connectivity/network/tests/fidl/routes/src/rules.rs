// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use assert_matches::assert_matches;
use fidl_fuchsia_net_routes_ext::{self as fnet_routes_ext, FidlRouteIpExt};
use fnet_routes_ext::admin::FidlRouteAdminIpExt;
use fnet_routes_ext::rules::{
    add_rule, close_rule_set, new_rule_set, remove_rule, FidlRuleAdminIpExt, FidlRuleIpExt,
    InstalledRule, RuleAction, RuleEvent, RuleIndex, RuleMatcher, DEFAULT_RULE_SET_PRIORITY,
};
use futures::TryStreamExt as _;
use net_types::ip::Ip;
use netstack_testing_common::realms::{Netstack3, TestSandboxExt};
use netstack_testing_macros::netstack_test;
use std::pin::pin;

// Verifies the watcher protocols correctly report `added` and `removed` events.
#[netstack_test]
#[variant(I, Ip)]
async fn rule_watcher_add_remove<
    I: Ip + FidlRouteIpExt + FidlRouteAdminIpExt + FidlRuleIpExt + FidlRuleAdminIpExt,
>(
    name: &str,
) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    // Rules are not supported in netstack2.
    let realm = sandbox.create_netstack_realm::<Netstack3, _>(name).expect("create realm");

    // Connect to the watcher protocol and consume all existing events.
    let state_proxy =
        realm.connect_to_protocol::<I::StateMarker>().expect("failed to connect to routes/State");
    let event_stream = fnet_routes_ext::rules::rule_event_stream_from_state::<I>(&state_proxy)
        .expect("failed to connect to routes watcher");
    let mut event_stream = pin!(event_stream);

    // There should be a default rule that points to the main table.
    let main_table =
        realm.connect_to_protocol::<I::RouteTableMarker>().expect("connect to main route table");
    let main_table_id =
        fnet_routes_ext::admin::get_table_id::<I>(&main_table).await.expect("get main table id");

    let default_rule = assert_matches!(
        event_stream.try_next().await,
        Ok(Some(RuleEvent::Existing(existing))) => existing
    );
    assert_eq!(
        default_rule,
        InstalledRule {
            priority: DEFAULT_RULE_SET_PRIORITY,
            index: RULE_INDEX_0,
            matcher: RuleMatcher::default(),
            action: RuleAction::Lookup(main_table_id),
        }
    );

    assert_matches!(event_stream.try_next().await, Ok(Some(RuleEvent::Idle)));

    let rule_table =
        realm.connect_to_protocol::<I::RuleTableMarker>().expect("connect to rule table");
    let priority = fnet_routes_ext::rules::RuleSetPriority::from(0);
    let rule_set = new_rule_set::<I>(&rule_table, priority).expect("fidl error");

    const RULE_INDEX_0: RuleIndex = RuleIndex::new(0);
    const RULE_INDEX_1: RuleIndex = RuleIndex::new(1);

    add_rule::<I>(&rule_set, RULE_INDEX_0, RuleMatcher::default(), RuleAction::Unreachable)
        .await
        .expect("fidl error")
        .expect("failed to add a new rule");
    let added = assert_matches!(
        event_stream.try_next().await,
        Ok(Some(RuleEvent::Added(added))) => added
    );
    assert_eq!(
        added,
        InstalledRule {
            priority,
            index: RULE_INDEX_0,
            matcher: RuleMatcher::default(),
            action: RuleAction::Unreachable,
        }
    );

    add_rule::<I>(&rule_set, RULE_INDEX_1, RuleMatcher::default(), RuleAction::Unreachable)
        .await
        .expect("fidl error")
        .expect("failed to add a new rule");

    let added = assert_matches!(
        event_stream.try_next().await,
        Ok(Some(RuleEvent::Added(added))) => added
    );
    assert_eq!(
        added,
        InstalledRule {
            priority,
            index: RULE_INDEX_1,
            matcher: RuleMatcher::default(),
            action: RuleAction::Unreachable,
        }
    );

    remove_rule::<I>(&rule_set, RULE_INDEX_0)
        .await
        .expect("fidl error")
        .expect("failed to remove an installed rule");
    let removed = assert_matches!(
        event_stream.try_next().await,
        Ok(Some(RuleEvent::Removed(removed))) => removed
    );
    assert_eq!(
        removed,
        InstalledRule {
            priority,
            index: RULE_INDEX_0,
            matcher: RuleMatcher::default(),
            action: RuleAction::Unreachable,
        }
    );

    close_rule_set::<I>(rule_set).await.expect("failed to remove rule set");
    let removed = assert_matches!(
        event_stream.try_next().await,
        Ok(Some(RuleEvent::Removed(removed))) => removed
    );
    assert_eq!(
        removed,
        InstalledRule {
            priority,
            index: RULE_INDEX_1,
            matcher: RuleMatcher::default(),
            action: RuleAction::Unreachable,
        }
    )
}
