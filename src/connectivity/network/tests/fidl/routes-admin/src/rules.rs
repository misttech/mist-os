// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use assert_matches::assert_matches;
use fidl::endpoints::ProtocolMarker;
use fidl::HandleBased;
use fidl_fuchsia_net_routes_ext::admin::FidlRouteAdminIpExt;
use fidl_fuchsia_net_routes_ext::rules::FidlRuleAdminIpExt;
use fidl_fuchsia_net_routes_ext::FidlRouteIpExt;
use fnet_routes_ext::rules::{RuleAction, RuleIndex, RuleSelector};
use futures::StreamExt as _;
use net_types::ip::{GenericOverIp, Ip, IpInvariant};
use netstack_testing_common::realms::{Netstack3, TestSandboxExt};
use netstack_testing_macros::netstack_test;
use routes_common::TestSetup;
use {
    fidl_fuchsia_net_routes_admin as fnet_routes_admin,
    fidl_fuchsia_net_routes_ext as fnet_routes_ext, fuchsia_zircon as zx,
};

fn rule_set_err_stream<I: FidlRuleAdminIpExt>(
    rule_set: <I::RuleSetMarker as ProtocolMarker>::Proxy,
) -> futures::stream::BoxStream<'static, fidl::Error> {
    #[derive(GenericOverIp)]
    #[generic_over_ip(I, Ip)]
    struct In<I: FidlRuleAdminIpExt>(<I::RuleSetMarker as ProtocolMarker>::Proxy);

    let IpInvariant(err_stream) = net_types::map_ip_twice!(I, In(rule_set), |In(rule_set)| {
        IpInvariant(
            rule_set
                .take_event_stream()
                .map(|result| match result {
                    Err(err) => err,
                    Ok(event) => match event {},
                })
                .boxed(),
        )
    });
    err_stream
}

#[netstack_test]
#[variant(I, Ip)]
async fn add_remove_rules<I: FidlRuleAdminIpExt + FidlRouteAdminIpExt + FidlRouteIpExt>(
    name: &str,
) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    // We don't support route rules in netstack2.
    let TestSetup {
        realm,
        network: _network,
        interface: _,
        route_table,
        global_route_table: _,
        state: _,
    } = TestSetup::<I>::new::<Netstack3>(&sandbox, name).await;
    let rule_table =
        realm.connect_to_protocol::<I::RuleTableMarker>().expect("connect to rule table");
    let priority = fnet_routes_ext::rules::RuleSetPriority::from(0);
    let rule_set =
        fnet_routes_ext::rules::new_rule_set::<I>(&rule_table, priority).expect("fidl error");

    const RULE_INDEX_0: RuleIndex = RuleIndex::new(0);
    const RULE_INDEX_1: RuleIndex = RuleIndex::new(1);

    fnet_routes_ext::rules::add_rule::<I>(
        &rule_set,
        RULE_INDEX_0,
        RuleSelector::default(),
        RuleAction::Unreachable,
    )
    .await
    .expect("fidl error")
    .expect("failed to add a new rule");

    assert_matches!(
        fnet_routes_ext::rules::add_rule::<I>(
            &rule_set,
            RULE_INDEX_0,
            RuleSelector::default(),
            RuleAction::Unreachable
        )
        .await,
        Ok(Err(fnet_routes_admin::RuleSetError::RuleAlreadyExists)),
        "cannot add a rule with an existing index"
    );

    // Adding a rule with a different index should succeed.
    fnet_routes_ext::rules::add_rule::<I>(
        &rule_set,
        RULE_INDEX_1,
        RuleSelector::default(),
        RuleAction::Unreachable,
    )
    .await
    .expect("fidl error")
    .expect("failed to add rule with the index back after the old rule is removed");

    fnet_routes_ext::rules::remove_rule::<I>(&rule_set, RuleIndex::from(0))
        .await
        .expect("fidl error")
        .expect("failed to remove a rule");

    assert_matches!(
        fnet_routes_ext::rules::remove_rule::<I>(
            &rule_set,
            fnet_routes_ext::rules::RuleIndex::from(0),
        )
        .await,
        Ok(Err(fnet_routes_admin::RuleSetError::RuleDoesNotExist)),
        "cannot remove a rule with a non-existing index"
    );

    fnet_routes_ext::rules::add_rule::<I>(
        &rule_set,
        RULE_INDEX_0,
        RuleSelector::default(),
        RuleAction::Unreachable,
    )
    .await
    .expect("fidl error")
    .expect("failed to add rule with the index back after the old rule is removed");

    // Cannot add the rule set at the same priority.
    let new_rule_set =
        fnet_routes_ext::rules::new_rule_set::<I>(&rule_table, priority).expect("fidl error");
    let mut err_stream = rule_set_err_stream::<I>(new_rule_set);
    assert_matches!(
        err_stream.next().await,
        Some(fidl::Error::ClientChannelClosed {
            status: zx::Status::ALREADY_EXISTS,
            protocol_name: _,
        })
    );
    assert_matches!(err_stream.next().await, None);

    fnet_routes_ext::rules::close_rule_set::<I>(rule_set).await.expect("fidl error");

    // Create a new rule set and we should be able to add a new rule.
    let new_rule_set =
        fnet_routes_ext::rules::new_rule_set::<I>(&rule_table, priority).expect("fidl error");

    let fnet_routes_admin::GrantForRouteTableAuthorization { table_id, token } =
        fnet_routes_ext::admin::get_authorization_for_route_table::<I>(&route_table)
            .await
            .expect("fidl error");

    assert_matches!(
        fnet_routes_ext::rules::add_rule::<I>(
            &new_rule_set,
            RULE_INDEX_0,
            RuleSelector::default(),
            RuleAction::Lookup(table_id),
        )
        .await,
        Ok(Err(fnet_routes_admin::RuleSetError::Unauthenticated)),
        "the rule set is not authenticated to the table"
    );

    fnet_routes_ext::rules::authenticate_for_route_table::<I>(&new_rule_set, table_id, token)
        .await
        .expect("fidl error")
        .expect("failed to authenticate");

    fnet_routes_ext::rules::add_rule::<I>(
        &new_rule_set,
        RULE_INDEX_0,
        RuleSelector::default(),
        RuleAction::Lookup(table_id),
    )
    .await
    .expect("fidl error")
    .expect("failed to add a new rule");
}

#[netstack_test]
#[variant(I, Ip)]
async fn invalid_interface_id_selector<
    I: FidlRuleAdminIpExt + FidlRouteAdminIpExt + FidlRouteIpExt,
>(
    name: &str,
) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    // We don't support route rules in netstack2.
    let realm = sandbox
        .create_netstack_realm::<Netstack3, _>(format!("routes-admin-{name}"))
        .expect("create realm");
    let rule_table =
        realm.connect_to_protocol::<I::RuleTableMarker>().expect("connect to rule table");
    let priority = fnet_routes_ext::rules::RuleSetPriority::from(0);
    let rule_set =
        fnet_routes_ext::rules::new_rule_set::<I>(&rule_table, priority).expect("fidl error");

    assert_matches!(
        fnet_routes_ext::rules::add_rule::<I>(
            &rule_set,
            RuleIndex::new(0),
            fnet_routes_ext::rules::RuleSelector {
                // Arbitrary non-existent interface ID.
                bound_device: Some(1001),
                ..Default::default()
            },
            fnet_routes_ext::rules::RuleAction::Unreachable
        )
        .await,
        Ok(Err(fnet_routes_admin::RuleSetError::BadInterfaceSelector))
    )
}

#[netstack_test]
#[variant(I, Ip)]
async fn bad_route_table_authentication<
    I: FidlRuleAdminIpExt + FidlRouteAdminIpExt + FidlRouteIpExt,
>(
    name: &str,
) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    // We don't support route rules in netstack2.
    let TestSetup {
        realm,
        network: _network,
        interface: _,
        route_table,
        global_route_table: _,
        state: _,
    } = TestSetup::<I>::new::<Netstack3>(&sandbox, name).await;
    let rule_table =
        realm.connect_to_protocol::<I::RuleTableMarker>().expect("connect to rule table");
    let rule_set = fnet_routes_ext::rules::new_rule_set::<I>(
        &rule_table,
        fnet_routes_ext::rules::RuleSetPriority::from(0),
    )
    .expect("fidl error");

    let fnet_routes_admin::GrantForRouteTableAuthorization { table_id, token } =
        fnet_routes_ext::admin::get_authorization_for_route_table::<I>(&route_table)
            .await
            .expect("fidl error");

    // Invalid table id because of version mismatch.
    assert_matches!(
        fnet_routes_ext::rules::authenticate_for_route_table::<I>(
            &rule_set,
            table_id + 1,
            token
                .duplicate_handle(fuchsia_zircon::Rights::SAME_RIGHTS)
                .expect("failed to duplicate token")
        )
        .await,
        Ok(Err(fnet_routes_admin::RuleSetError::BadAuthentication))
    );

    // Non-existent table that matches the IP version.
    assert_matches!(
        fnet_routes_ext::rules::authenticate_for_route_table::<I>(&rule_set, table_id + 2, token)
            .await,
        Ok(Err(fnet_routes_admin::RuleSetError::BadAuthentication))
    );

    // Wrong token.
    assert_matches!(
        fnet_routes_ext::rules::authenticate_for_route_table::<I>(
            &rule_set,
            table_id,
            zx::Event::create(),
        )
        .await,
        Ok(Err(fnet_routes_admin::RuleSetError::BadAuthentication))
    );
}
