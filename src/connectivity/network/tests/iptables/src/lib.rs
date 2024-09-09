// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![cfg(test)]

use std::collections::HashMap;
use std::pin::pin;

use component_events::events::{EventStream, ExitStatus, Stopped, StoppedPayload};
use component_events::matcher::EventMatcher;
use fidl_fuchsia_net_filter_ext::{
    Action, ControllerId, Domain, Matchers, Namespace, NamespaceId, Resource, ResourceId, Routine,
    RoutineId, RoutineType, Rule, RuleId,
};
use fuchsia_component_test::{RealmBuilder, RealmBuilderParams, RealmInstance};
use test_case::test_case;
use {
    fidl_fuchsia_data as fdata, fidl_fuchsia_net_filter as fnet_filter,
    fidl_fuchsia_net_filter_ext as fnet_filter_ext,
};

async fn run_iptables(args: impl IntoIterator<Item = &'static str>) -> RealmInstance {
    let mut events = EventStream::open().await.unwrap();

    let builder =
        RealmBuilder::with_params(RealmBuilderParams::new().from_relative_url("#meta/realm.cm"))
            .await
            .expect("create realm builder");

    let args = args.into_iter().map(String::from).collect();
    let mut decl = builder.get_component_decl("iptables").await.expect("get component decl");
    *decl
        .program
        .as_mut()
        .expect("component should have program section")
        .info
        .entries
        .as_mut()
        .expect("dictionary entries should be specified")
        .iter_mut()
        .find_map(|fdata::DictionaryEntry { key, value }| (key == "args").then_some(value))
        .expect("component should have args") =
        Some(Box::new(fdata::DictionaryValue::StrVec(args)));
    builder.replace_component_decl("iptables", decl).await.expect("replace component decl");

    let realm = builder.build().await.unwrap();

    let event = EventMatcher::ok()
        .moniker(format!("realm_builder:{}/iptables", realm.root.child_name()))
        .stop(None)
        .wait::<Stopped>(&mut events)
        .await
        .expect("wait for stopped event");
    let StoppedPayload { status, .. } = event.result().expect("extract event payload");
    assert_eq!(status, &ExitStatus::Clean);

    realm
}

// Starnix's iptables subsystem prefixes table names with their IP version so
// that the IPv4 and IPv6 versions of a table do not conflict;
// fuchsia.net.filter requires that all namespaces owned by the same controller
// have unique names.
fn namespace_id_for_ipv4_table(table: &str) -> NamespaceId {
    NamespaceId(format!("ipv4-{table}"))
}

#[fuchsia::test]
async fn version() {
    let _realm = run_iptables(["--version"]).await;
}

#[test_case("filter", RoutineType::Ip(None); "filter chain")]
#[test_case("mangle", RoutineType::Ip(None); "mangle chain")]
#[test_case("nat", RoutineType::Nat(None); "nat chain")]
#[fuchsia::test]
async fn create_chain(table: &'static str, routine_type: RoutineType) {
    let realm = run_iptables(["--table", table, "--new-chain", "test"]).await;

    let state = realm
        .root
        .connect_to_protocol_at_exposed_dir::<fnet_filter::StateMarker>()
        .expect("connect to protocol");
    let stream = fnet_filter_ext::event_stream_from_state(state).expect("get filter event stream");
    let mut stream = pin!(stream);
    let observed: HashMap<_, _> =
        fnet_filter_ext::get_existing_resources(&mut stream).await.expect("get resources");
    let starnix = observed
        .get(&ControllerId(String::from("starnix")))
        .expect("starnix should create controller");

    let namespace = namespace_id_for_ipv4_table(table);
    let routine = RoutineId { namespace: namespace.clone(), name: String::from("test") };
    let rule = RuleId { routine: routine.clone(), index: 0 };
    let expected = [
        (
            ResourceId::Namespace(namespace.clone()),
            Resource::Namespace(Namespace { id: namespace, domain: Domain::Ipv4 }),
        ),
        (
            ResourceId::Routine(routine.clone()),
            Resource::Routine(Routine { id: routine, routine_type }),
        ),
        (
            ResourceId::Rule(rule.clone()),
            Resource::Rule(Rule {
                id: rule,
                matchers: Matchers::default(),
                action: Action::Return,
            }),
        ),
    ];

    for (expected_id, expected_resource) in expected {
        let observed_resource = starnix
            .get(&expected_id)
            .expect("expected resource {expected_resource:#?}; did not find in {starnix:#?}");
        assert_eq!(observed_resource, &expected_resource);
    }
}

// TODO(https://fxbug.dev/307908515): observe and append rules to built-in chains.

// TODO(https://fxbug.dev/307908515): test ip6tables once it is supported in starnix.
