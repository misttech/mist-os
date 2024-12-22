// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![cfg(test)]

use std::collections::HashMap;
use std::pin::pin;

use component_events::events::{EventStream, ExitStatus, Stopped, StoppedPayload};
use component_events::matcher::EventMatcher;
use fidl_fuchsia_net_filter_ext::{
    Action, ControllerId, Domain, InstalledIpRoutine, InstalledNatRoutine, IpHook, Matchers,
    Namespace, NamespaceId, NatHook, Resource, Routine, RoutineId, RoutineType, Rule, RuleId,
};
use fuchsia_component_test::{RealmBuilder, RealmBuilderParams, RealmInstance};
use fuchsia_runtime::{HandleInfo, HandleType};
use log::info;
use test_case::test_case;
use {
    fidl_fuchsia_component as fcomponent, fidl_fuchsia_component_decl as fcomponent_decl,
    fidl_fuchsia_net_filter as fnet_filter, fidl_fuchsia_net_filter_ext as fnet_filter_ext,
    fidl_fuchsia_process as fprocess,
};

const IPTABLES_RESTORE: &'static str = "iptables-restore";
const IP6TABLES_RESTORE: &'static str = "ip6tables-restore";

const FILTER_TABLE_WITH_CUSTOM_CHAIN: &[&str] = &[
    "*filter",
    ":INPUT ACCEPT [0:0]",
    ":FORWARD ACCEPT [0:0]",
    ":OUTPUT ACCEPT [0:0]",
    ":test -",
    "COMMIT",
];

const NAT_TABLE_WITH_CUSTOM_CHAIN: &[&str] = &[
    "*nat",
    ":PREROUTING ACCEPT [0:0]",
    ":INPUT ACCEPT [0:0]",
    ":OUTPUT ACCEPT [0:0]",
    ":POSTROUTING ACCEPT [0:0]",
    ":test -",
    "COMMIT",
];

const MANGLE_TABLE_WITH_CUSTOM_CHAIN: &[&str] = &[
    "*mangle",
    ":PREROUTING ACCEPT [0:0]",
    ":INPUT ACCEPT [0:0]",
    ":FORWARD ACCEPT [0:0]",
    ":OUTPUT ACCEPT [0:0]",
    ":POSTROUTING ACCEPT [0:0]",
    ":test -",
    "COMMIT",
];

/// Runs component `name` in Realm collection "test-programs" and with `input_lines` as stdin.
/// Items in `input_lines` are suffixed with newline character, and sent one line at a time, and
/// then stdin is closed. Checks that component exited with `ExitStatus::Clean` and returns the
/// realm.
/// Panics if any errors are encountered.
async fn run_with_input(name: &'static str, input_lines: &[&'static str]) -> RealmInstance {
    let mut events = EventStream::open().await.unwrap();
    let builder =
        RealmBuilder::with_params(RealmBuilderParams::new().from_relative_url("#meta/realm.cm"))
            .await
            .expect("create realm builder");

    let realm = builder.build().await.unwrap();
    let test_realm =
        realm.root.connect_to_protocol_at_exposed_dir::<fcomponent::RealmMarker>().unwrap();

    info!("Running {name}");

    let (stdin_recv, stdin_send) = zx::Socket::create_stream();
    test_realm
        .create_child(
            &fcomponent_decl::CollectionRef { name: "test-programs".to_string() },
            &fcomponent_decl::Child {
                name: Some(name.to_string()),
                url: Some(format!("#meta/{name}.cm")),
                startup: Some(fcomponent_decl::StartupMode::Eager),
                ..Default::default()
            },
            fcomponent::CreateChildArgs {
                numbered_handles: Some(vec![fprocess::HandleInfo {
                    id: HandleInfo::new(HandleType::FileDescriptor, libc::STDIN_FILENO as u16)
                        .as_raw(),
                    handle: stdin_recv.into(),
                }]),
                ..Default::default()
            },
        )
        .await
        .expect("fidl call to fuchsia.component.Realm/CreateChild")
        .expect("CreateChild successfully creates child component in collection");

    for line in input_lines {
        info!("{name}: {line}");
        let bytes = format!("{line}\n").into_bytes();
        assert_eq!(stdin_send.write(&bytes).expect("write to stdin"), bytes.len());
    }

    drop(stdin_send);

    let event = EventMatcher::ok()
        .moniker(format!("realm_builder:{}/test-programs:{name}", realm.root.child_name()))
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
fn namespace_id_for_ipv6_table(table: &str) -> NamespaceId {
    NamespaceId(format!("ipv6-{table}"))
}

fn filter_table_with_custom_chain_resources(namespace: Namespace) -> Vec<Resource> {
    let namespace_id = namespace.id.clone();
    let priority = 0;
    let input_routine_id =
        RoutineId { namespace: namespace_id.clone(), name: String::from("INPUT") };
    let forward_routine_id =
        RoutineId { namespace: namespace_id.clone(), name: String::from("FORWARD") };
    let output_routine_id =
        RoutineId { namespace: namespace_id.clone(), name: String::from("OUTPUT") };
    let custom_routine_id =
        RoutineId { namespace: namespace_id.clone(), name: String::from("test") };
    vec![
        Resource::Namespace(namespace),
        // INPUT built-in routine
        Resource::Routine(Routine {
            id: input_routine_id.clone(),
            routine_type: RoutineType::Ip(Some(InstalledIpRoutine {
                hook: IpHook::LocalIngress,
                priority,
            })),
        }),
        Resource::Rule(Rule {
            id: RuleId { routine: input_routine_id, index: 0 },
            matchers: Matchers::default(),
            action: Action::Accept,
        }),
        // FORWARD built-in routine
        Resource::Routine(Routine {
            id: forward_routine_id.clone(),
            routine_type: RoutineType::Ip(Some(InstalledIpRoutine {
                hook: IpHook::Forwarding,
                priority,
            })),
        }),
        Resource::Rule(Rule {
            id: RuleId { routine: forward_routine_id, index: 0 },
            matchers: Matchers::default(),
            action: Action::Accept,
        }),
        // OUTPUT built-in routine
        Resource::Routine(Routine {
            id: output_routine_id.clone(),
            routine_type: RoutineType::Ip(Some(InstalledIpRoutine {
                hook: IpHook::LocalEgress,
                priority,
            })),
        }),
        Resource::Rule(Rule {
            id: RuleId { routine: output_routine_id, index: 0 },
            matchers: Matchers::default(),
            action: Action::Accept,
        }),
        // Custom routine
        Resource::Routine(Routine {
            id: custom_routine_id.clone(),
            routine_type: RoutineType::Ip(None),
        }),
        Resource::Rule(Rule {
            id: RuleId { routine: custom_routine_id, index: 0 },
            matchers: Matchers::default(),
            action: Action::Return,
        }),
    ]
}

fn nat_table_with_custom_chain_resources(namespace: Namespace) -> Vec<Resource> {
    let namespace_id = namespace.id.clone();
    let dnat_priority = -100;
    let snat_priority = 100;
    let prerouting_routine_id =
        RoutineId { namespace: namespace_id.clone(), name: String::from("PREROUTING") };
    let input_routine_id =
        RoutineId { namespace: namespace_id.clone(), name: String::from("INPUT") };
    let output_routine_id =
        RoutineId { namespace: namespace_id.clone(), name: String::from("OUTPUT") };
    let postrouting_routine_id =
        RoutineId { namespace: namespace_id.clone(), name: String::from("POSTROUTING") };
    let custom_routine_id =
        RoutineId { namespace: namespace_id.clone(), name: String::from("test") };
    vec![
        Resource::Namespace(namespace),
        // PREROUTING built-in routine
        Resource::Routine(Routine {
            id: prerouting_routine_id.clone(),
            routine_type: RoutineType::Nat(Some(InstalledNatRoutine {
                hook: NatHook::Ingress,
                priority: dnat_priority,
            })),
        }),
        Resource::Rule(Rule {
            id: RuleId { routine: prerouting_routine_id, index: 0 },
            matchers: Matchers::default(),
            action: Action::Accept,
        }),
        // INPUT built-in routine
        Resource::Routine(Routine {
            id: input_routine_id.clone(),
            routine_type: RoutineType::Nat(Some(InstalledNatRoutine {
                hook: NatHook::LocalIngress,
                priority: snat_priority,
            })),
        }),
        Resource::Rule(Rule {
            id: RuleId { routine: input_routine_id, index: 0 },
            matchers: Matchers::default(),
            action: Action::Accept,
        }),
        // OUTPUT built-in routine
        Resource::Routine(Routine {
            id: output_routine_id.clone(),
            routine_type: RoutineType::Nat(Some(InstalledNatRoutine {
                hook: NatHook::LocalEgress,
                priority: dnat_priority,
            })),
        }),
        Resource::Rule(Rule {
            id: RuleId { routine: output_routine_id, index: 0 },
            matchers: Matchers::default(),
            action: Action::Accept,
        }),
        // POSTROUTING built-in routine
        Resource::Routine(Routine {
            id: postrouting_routine_id.clone(),
            routine_type: RoutineType::Nat(Some(InstalledNatRoutine {
                hook: NatHook::Egress,
                priority: snat_priority,
            })),
        }),
        Resource::Rule(Rule {
            id: RuleId { routine: postrouting_routine_id, index: 0 },
            matchers: Matchers::default(),
            action: Action::Accept,
        }),
        // Custom routine
        Resource::Routine(Routine {
            id: custom_routine_id.clone(),
            routine_type: RoutineType::Nat(None),
        }),
        Resource::Rule(Rule {
            id: RuleId { routine: custom_routine_id, index: 0 },
            matchers: Matchers::default(),
            action: Action::Return,
        }),
    ]
}

fn mangle_table_with_custom_chain_resources(namespace: Namespace) -> Vec<Resource> {
    let namespace_id = namespace.id.clone();
    let priority = -150;
    let prerouting_routine_id =
        RoutineId { namespace: namespace_id.clone(), name: String::from("PREROUTING") };
    let input_routine_id =
        RoutineId { namespace: namespace_id.clone(), name: String::from("INPUT") };
    let forward_routine_id =
        RoutineId { namespace: namespace_id.clone(), name: String::from("FORWARD") };
    let output_routine_id =
        RoutineId { namespace: namespace_id.clone(), name: String::from("OUTPUT") };
    let postrouting_routine_id =
        RoutineId { namespace: namespace_id.clone(), name: String::from("POSTROUTING") };
    let custom_routine_id =
        RoutineId { namespace: namespace_id.clone(), name: String::from("test") };
    vec![
        Resource::Namespace(namespace),
        // PREROUTING built-in routine
        Resource::Routine(Routine {
            id: prerouting_routine_id.clone(),
            routine_type: RoutineType::Ip(Some(InstalledIpRoutine {
                hook: IpHook::Ingress,
                priority,
            })),
        }),
        Resource::Rule(Rule {
            id: RuleId { routine: prerouting_routine_id, index: 0 },
            matchers: Matchers::default(),
            action: Action::Accept,
        }),
        // INPUT built-in routine
        Resource::Routine(Routine {
            id: input_routine_id.clone(),
            routine_type: RoutineType::Ip(Some(InstalledIpRoutine {
                hook: IpHook::LocalIngress,
                priority,
            })),
        }),
        Resource::Rule(Rule {
            id: RuleId { routine: input_routine_id, index: 0 },
            matchers: Matchers::default(),
            action: Action::Accept,
        }),
        // FORWARD built-in routine
        Resource::Routine(Routine {
            id: forward_routine_id.clone(),
            routine_type: RoutineType::Ip(Some(InstalledIpRoutine {
                hook: IpHook::Forwarding,
                priority,
            })),
        }),
        Resource::Rule(Rule {
            id: RuleId { routine: forward_routine_id, index: 0 },
            matchers: Matchers::default(),
            action: Action::Accept,
        }),
        // OUTPUT built-in routine
        Resource::Routine(Routine {
            id: output_routine_id.clone(),
            routine_type: RoutineType::Ip(Some(InstalledIpRoutine {
                hook: IpHook::LocalEgress,
                priority,
            })),
        }),
        Resource::Rule(Rule {
            id: RuleId { routine: output_routine_id, index: 0 },
            matchers: Matchers::default(),
            action: Action::Accept,
        }),
        // POSTROUTING built-in routine
        Resource::Routine(Routine {
            id: postrouting_routine_id.clone(),
            routine_type: RoutineType::Ip(Some(InstalledIpRoutine {
                hook: IpHook::Egress,
                priority,
            })),
        }),
        Resource::Rule(Rule {
            id: RuleId { routine: postrouting_routine_id, index: 0 },
            matchers: Matchers::default(),
            action: Action::Accept,
        }),
        // Custom routine
        Resource::Routine(Routine {
            id: custom_routine_id.clone(),
            routine_type: RoutineType::Ip(None),
        }),
        Resource::Rule(Rule {
            id: RuleId { routine: custom_routine_id, index: 0 },
            matchers: Matchers::default(),
            action: Action::Return,
        }),
    ]
}

enum Ip {
    V4,
    V6,
}

#[test_case(
    Ip::V4,
    "filter",
    FILTER_TABLE_WITH_CUSTOM_CHAIN,
    filter_table_with_custom_chain_resources;
    "filter chain ipv4"
)]
#[test_case(
    Ip::V6,
    "filter",
    FILTER_TABLE_WITH_CUSTOM_CHAIN,
    filter_table_with_custom_chain_resources;
    "filter chain ipv6"
)]
#[test_case(
    Ip::V4,
    "nat",
    NAT_TABLE_WITH_CUSTOM_CHAIN,
    nat_table_with_custom_chain_resources;
    "nat chain ipv4"
)]
#[test_case(
    Ip::V6,
    "nat",
    NAT_TABLE_WITH_CUSTOM_CHAIN,
    nat_table_with_custom_chain_resources;
    "nat chain ipv6"
)]
#[test_case(
    Ip::V4,
    "mangle",
    MANGLE_TABLE_WITH_CUSTOM_CHAIN,
    mangle_table_with_custom_chain_resources;
    "mangle chain ipv4"
)]
#[test_case(
    Ip::V6,
    "mangle",
    MANGLE_TABLE_WITH_CUSTOM_CHAIN,
    mangle_table_with_custom_chain_resources;
    "mangle chain ipv6"
)]
#[fuchsia::test]
async fn create_chain(
    protocol: Ip,
    table_name: &str,
    table_spec: &[&'static str],
    expected_resources_fn: fn(Namespace) -> Vec<Resource>,
) {
    let (binary, namespace) = match protocol {
        Ip::V4 => (
            IPTABLES_RESTORE,
            Namespace { id: namespace_id_for_ipv4_table(table_name), domain: Domain::Ipv4 },
        ),
        Ip::V6 => (
            IP6TABLES_RESTORE,
            Namespace { id: namespace_id_for_ipv6_table(table_name), domain: Domain::Ipv6 },
        ),
    };

    let realm = run_with_input(binary, table_spec).await;

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

    for expected_resource in expected_resources_fn(namespace) {
        let observed_resource = starnix
            .get(&expected_resource.id())
            .expect("expected resource {expected_resource:#?}; did not find in {starnix:#?}");
        assert_eq!(observed_resource, &expected_resource);
    }
}

// TODO(https://fxbug.dev/307908515): observe and append rules to built-in chains.

// TODO(https://fxbug.dev/307908515): test ip6tables once it is supported in starnix.
