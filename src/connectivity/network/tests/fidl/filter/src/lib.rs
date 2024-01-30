// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![cfg(test)]

use assert_matches::assert_matches;
use fidl::endpoints::Proxy as _;
use fidl_fuchsia_net as fnet;
use fidl_fuchsia_net_filter as fnet_filter;
use fidl_fuchsia_net_filter_ext as fnet_filter_ext;
use fuchsia_async::{DurationExt as _, TimeoutExt as _};
use futures::{FutureExt as _, StreamExt as _};
use itertools::Itertools as _;
use net_declare::{fidl_ip, fidl_subnet};
use netstack_testing_common::{
    realms::{Netstack3, TestSandboxExt as _},
    ASYNC_EVENT_NEGATIVE_CHECK_TIMEOUT,
};
use netstack_testing_macros::netstack_test;
use std::collections::{HashMap, HashSet};
use test_case::test_case;

trait TestValue {
    fn test_value() -> Self;
}

impl TestValue for fnet_filter_ext::ResourceId {
    fn test_value() -> Self {
        Self::Namespace(fnet_filter_ext::NamespaceId::test_value())
    }
}

impl TestValue for fnet_filter_ext::Resource {
    fn test_value() -> Self {
        Self::Namespace(fnet_filter_ext::Namespace::test_value())
    }
}

impl TestValue for fnet_filter_ext::NamespaceId {
    fn test_value() -> Self {
        Self("NAMESPACE_ID".to_owned())
    }
}

impl TestValue for fnet_filter_ext::Namespace {
    fn test_value() -> Self {
        Self {
            id: fnet_filter_ext::NamespaceId::test_value(),
            domain: fnet_filter_ext::Domain::AllIp,
        }
    }
}

impl TestValue for fnet_filter_ext::RoutineId {
    fn test_value() -> Self {
        Self {
            namespace: fnet_filter_ext::NamespaceId::test_value(),
            name: String::from("ingress"),
        }
    }
}

impl TestValue for fnet_filter_ext::Routine {
    fn test_value() -> Self {
        Self {
            id: fnet_filter_ext::RoutineId::test_value(),
            routine_type: fnet_filter_ext::RoutineType::Ip(Some(
                fnet_filter_ext::InstalledIpRoutine {
                    hook: fnet_filter_ext::IpHook::Ingress,
                    priority: 0,
                },
            )),
        }
    }
}

impl TestValue for fnet_filter_ext::RuleId {
    fn test_value() -> Self {
        Self { routine: fnet_filter_ext::RoutineId::test_value(), index: 0 }
    }
}

impl TestValue for fnet_filter_ext::Rule {
    fn test_value() -> Self {
        fnet_filter_ext::Rule {
            id: fnet_filter_ext::RuleId {
                routine: fnet_filter_ext::RoutineId::test_value(),
                index: 0,
            },
            matchers: fnet_filter_ext::Matchers {
                transport_protocol: Some(fnet_filter_ext::TransportProtocolMatcher::Tcp {
                    src_port: None,
                    dst_port: Some(
                        fnet_filter_ext::PortMatcher::new(22, 22, /* invert */ false)
                            .expect("valid port range"),
                    ),
                }),
                ..Default::default()
            },
            action: fnet_filter_ext::Action::Drop,
        }
    }
}

#[netstack_test]
async fn watcher_existing(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let realm = sandbox.create_netstack_realm::<Netstack3, _>(name).expect("create realm");
    let state =
        realm.connect_to_protocol::<fnet_filter::StateMarker>().expect("connect to protocol");

    // There should be no resources on startup.
    {
        let stream = fnet_filter_ext::event_stream_from_state(state.clone())
            .expect("get filter event stream");
        futures::pin_mut!(stream);
        let observed: HashMap<_, _> =
            fnet_filter_ext::get_existing_resources(&mut stream).await.expect("get resources");
        assert_eq!(observed, HashMap::new());
    }

    let control =
        realm.connect_to_protocol::<fnet_filter::ControlMarker>().expect("connect to protocol");
    let mut controller =
        fnet_filter_ext::Controller::new(&control, &fnet_filter_ext::ControllerId(name.to_owned()))
            .await
            .expect("create controller");

    let resources = [
        fnet_filter_ext::Resource::Namespace(fnet_filter_ext::Namespace::test_value()),
        fnet_filter_ext::Resource::Routine(fnet_filter_ext::Routine::test_value()),
        fnet_filter_ext::Resource::Rule(fnet_filter_ext::Rule::test_value()),
    ];
    controller
        .push_changes(resources.iter().cloned().map(fnet_filter_ext::Change::Create).collect())
        .await
        .expect("push changes");
    controller.commit().await.expect("commit pending changes");

    let stream = fnet_filter_ext::event_stream_from_state(state).expect("get filter event stream");
    futures::pin_mut!(stream);
    let observed: HashMap<_, _> =
        fnet_filter_ext::get_existing_resources(&mut stream).await.expect("get resources");
    assert_eq!(
        observed,
        HashMap::from([(
            controller.id().clone(),
            resources
                .into_iter()
                .map(|resource| (resource.id(), resource))
                .collect::<HashMap<_, _>>(),
        )])
    );
}

#[netstack_test]
async fn watcher_observe_updates(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let realm = sandbox.create_netstack_realm::<Netstack3, _>(name).expect("create realm");

    let state =
        realm.connect_to_protocol::<fnet_filter::StateMarker>().expect("connect to protocol");
    let stream = fnet_filter_ext::event_stream_from_state(state).expect("get filter event stream");
    futures::pin_mut!(stream);
    assert_eq!(
        stream.next().await.expect("wait for idle").expect("wait for idle"),
        fnet_filter_ext::Event::Idle
    );

    let control =
        realm.connect_to_protocol::<fnet_filter::ControlMarker>().expect("connect to protocol");
    let mut controller =
        fnet_filter_ext::Controller::new(&control, &fnet_filter_ext::ControllerId(name.to_owned()))
            .await
            .expect("create controller");

    let resources = [
        fnet_filter_ext::Resource::Namespace(fnet_filter_ext::Namespace::test_value()),
        fnet_filter_ext::Resource::Routine(fnet_filter_ext::Routine::test_value()),
        fnet_filter_ext::Resource::Rule(fnet_filter_ext::Rule::test_value()),
    ];
    controller
        .push_changes(resources.iter().cloned().map(fnet_filter_ext::Change::Create).collect())
        .await
        .expect("push changes");

    assert_matches!(
        stream.next().on_timeout(ASYNC_EVENT_NEGATIVE_CHECK_TIMEOUT.after_now(), || None).await,
        None,
        "changes should not be broadcast until committed"
    );

    controller.commit().await.expect("commit pending changes");
    for resource in &resources {
        let (controller_id, added_resource) = assert_matches!(
            stream.next().await,
            Some(Ok(fnet_filter_ext::Event::Added(id, resource))) => (id, resource),
            "added resources should be broadcast to watcher"
        );
        assert_eq!(&controller_id, controller.id());
        assert_eq!(&added_resource, resource);
    }
    assert_matches!(
        stream.next().await,
        Some(Ok(fnet_filter_ext::Event::EndOfUpdate)),
        "transactional updates should be demarcated with EndOfUpdate event"
    );

    // Removing a containing resource (e.g. a namespace) also removes all its
    // contents.
    let to_remove = fnet_filter_ext::Resource::Namespace(fnet_filter_ext::Namespace::test_value());
    controller
        .push_changes(vec![fnet_filter_ext::Change::Remove(to_remove.id())])
        .await
        .expect("push changes");
    controller.commit().await.expect("commit pending changes");
    let mut expected = resources.into_iter().map(|resource| resource.id()).collect::<HashSet<_>>();
    while !expected.is_empty() {
        let (controller_id, removed_resource) = assert_matches!(
            stream.next().await,
            Some(Ok(fnet_filter_ext::Event::Removed(id, resource))) => (id, resource),
            "removed resources should be broadcast to watcher"
        );
        assert_eq!(&controller_id, controller.id());
        assert_eq!(expected.remove(&removed_resource), true);
    }
    assert_matches!(
        stream.next().await,
        Some(Ok(fnet_filter_ext::Event::EndOfUpdate)),
        "transactional updates should be demarcated with EndOfUpdate event"
    );
}

#[netstack_test]
async fn resources_and_events_scoped_to_controllers(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let realm = sandbox.create_netstack_realm::<Netstack3, _>(name).expect("create realm");

    let state =
        realm.connect_to_protocol::<fnet_filter::StateMarker>().expect("connect to protocol");
    let stream = fnet_filter_ext::event_stream_from_state(state).expect("get filter event stream");
    futures::pin_mut!(stream);
    assert_eq!(
        stream.next().await.expect("wait for idle").expect("wait for idle"),
        fnet_filter_ext::Event::Idle
    );

    let control =
        realm.connect_to_protocol::<fnet_filter::ControlMarker>().expect("connect to protocol");
    let create_controller_and_commit_updates = |name: &'static str| async {
        let mut controller = fnet_filter_ext::Controller::new(
            &control,
            &fnet_filter_ext::ControllerId(name.to_owned()),
        )
        .await
        .expect("create controller");
        controller
            .push_changes(vec![fnet_filter_ext::Change::Create(
                fnet_filter_ext::Resource::test_value(),
            )])
            .await
            .expect("push changes");
        controller.commit().await.expect("commit pending changes");
        controller
    };

    // Add two identical resources under different controllers. Note that we
    // retain the controllers as dropping them would cause their resources to be
    // removed.
    let _controllers = futures::future::join_all([
        create_controller_and_commit_updates("controller-a"),
        create_controller_and_commit_updates("controller-b"),
    ])
    .await;

    let mut expected_controllers = HashSet::from(["controller-a", "controller-b"]);
    while !expected_controllers.is_empty() {
        let (fnet_filter_ext::ControllerId(id), added_resource) = assert_matches!(
            stream.next().await,
            Some(Ok(fnet_filter_ext::Event::Added(id, resource))) => (id, resource),
            "added resources should be broadcast to watcher"
        );
        assert!(expected_controllers.remove(id.as_str()));
        assert_eq!(added_resource, fnet_filter_ext::Resource::test_value());
        assert_matches!(
            stream.next().await,
            Some(Ok(fnet_filter_ext::Event::EndOfUpdate)),
            "transactional updates should be demarcated with EndOfUpdate event"
        );
    }
}

#[netstack_test]
async fn watcher_already_pending(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let realm = sandbox.create_netstack_realm::<Netstack3, _>(name).expect("create realm");
    let state =
        realm.connect_to_protocol::<fnet_filter::StateMarker>().expect("connect to protocol");
    let (watcher, server_end) =
        fidl::endpoints::create_proxy::<fnet_filter::WatcherMarker>().expect("create proxy");
    state.get_watcher(&fnet_filter::WatcherOptions::default(), server_end).expect("get watcher");

    let events = watcher.watch().await.expect("get existing resources");
    assert_eq!(events, &[fnet_filter::Event::Idle(fnet_filter::Empty {})]);

    // Call `Watch` twice and the netstack should close the channel.
    assert_matches!(
        futures::future::join(watcher.watch(), watcher.watch()).await,
        (
            Err(fidl::Error::ClientChannelClosed { status: fidl::Status::PEER_CLOSED, .. }),
            Err(fidl::Error::ClientChannelClosed { status: fidl::Status::PEER_CLOSED, .. }),
        )
    );
    assert!(watcher.is_closed());
}

#[netstack_test]
async fn watcher_channel_closed_if_not_polled(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let realm = sandbox.create_netstack_realm::<Netstack3, _>(name).expect("create realm");
    let state =
        realm.connect_to_protocol::<fnet_filter::StateMarker>().expect("connect to protocol");
    let (watcher, server_end) =
        fidl::endpoints::create_proxy::<fnet_filter::WatcherMarker>().expect("create proxy");
    state.get_watcher(&fnet_filter::WatcherOptions::default(), server_end).expect("get watcher");

    let events = watcher.watch().await.expect("get existing resources");
    assert_eq!(events, &[fnet_filter::Event::Idle(fnet_filter::Empty {})]);

    let control =
        realm.connect_to_protocol::<fnet_filter::ControlMarker>().expect("connect to protocol");
    let mut controller = fnet_filter_ext::Controller::new(
        &control,
        &fnet_filter_ext::ControllerId(String::from("test")),
    )
    .await
    .expect("create controller");

    async fn create_and_remove_namespace(controller: &mut fnet_filter_ext::Controller) {
        controller
            .push_changes(vec![fnet_filter_ext::Change::Create(
                fnet_filter_ext::Resource::test_value(),
            )])
            .await
            .expect("push changes");
        controller.commit().await.expect("commit pending changes");

        controller
            .push_changes(vec![fnet_filter_ext::Change::Remove(
                fnet_filter_ext::ResourceId::test_value(),
            )])
            .await
            .expect("push changes");
        controller.commit().await.expect("commit pending changes");
    }

    // Repeatedly add and remove resources, causing events to be queued
    // server-side for the watcher.
    let perform_updates = async {
        loop {
            create_and_remove_namespace(&mut controller).await
        }
    }
    .fuse();
    futures::pin_mut!(perform_updates);

    // Wait for the watcher channel to be closed as a result.
    let mut event_stream = watcher.take_event_stream();
    futures::select! {
        event = event_stream.next() => assert_matches!(event, None),
        _ = perform_updates => unreachable!(),
    }
}

#[netstack_test]
async fn on_id_assigned(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let realm = sandbox.create_netstack_realm::<Netstack3, _>(name).expect("create realm");
    let control =
        realm.connect_to_protocol::<fnet_filter::ControlMarker>().expect("connect to protocol");

    let controller_id = fnet_filter_ext::ControllerId(String::from("test"));
    let open_new_controller = || async {
        fnet_filter_ext::Controller::new(&control, &controller_id).await.expect("create controller")
    };

    let mut controller = open_new_controller().await;
    assert_eq!(controller.id(), &controller_id);

    // The netstack should deduplicate IDs if there are conflicts.
    let other_controller = open_new_controller().await;
    assert_ne!(other_controller.id(), &controller_id);

    // Add a resource with the first controller and initialize a watcher so that
    // we'll be able to observe its removal.
    let resource = fnet_filter_ext::Resource::test_value();
    controller
        .push_changes(vec![fnet_filter_ext::Change::Create(resource.clone())])
        .await
        .expect("push changes");
    controller.commit().await.expect("commit pending changes");
    let state =
        realm.connect_to_protocol::<fnet_filter::StateMarker>().expect("connect to protocol");
    let stream = fnet_filter_ext::event_stream_from_state(state).expect("get filter event stream");
    futures::pin_mut!(stream);
    let mut observed: HashMap<_, _> =
        fnet_filter_ext::get_existing_resources(&mut stream).await.expect("get resources");
    assert_eq!(
        observed,
        HashMap::from([(
            controller.id().clone(),
            HashMap::from([(resource.id(), resource.clone())])
        )])
    );

    // If the first controller is closed, its ID can be reused.
    //
    // NB: to avoid a race between the server-side handling of the channel
    // closure and opening a new controller with the same ID, we wait to observe
    // removal of the controller's resources.
    drop(controller);
    fnet_filter_ext::wait_for_condition(&mut stream, &mut observed, |state| {
        state.get(&controller_id).unwrap().is_empty()
    })
    .await
    .expect("controller's resources should be removed on drop");

    let controller = open_new_controller().await;
    assert_eq!(controller.id(), &controller_id);
}

#[netstack_test]
async fn drop_controller_removes_resources(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let realm = sandbox.create_netstack_realm::<Netstack3, _>(name).expect("create realm");

    let control =
        realm.connect_to_protocol::<fnet_filter::ControlMarker>().expect("connect to protocol");
    let controller_id = fnet_filter_ext::ControllerId(String::from("test"));
    let mut controller = fnet_filter_ext::Controller::new(&control, &controller_id)
        .await
        .expect("create controller");

    // Create some resources with the controller.
    let resources = [
        fnet_filter_ext::Resource::Namespace(fnet_filter_ext::Namespace::test_value()),
        fnet_filter_ext::Resource::Routine(fnet_filter_ext::Routine::test_value()),
        fnet_filter_ext::Resource::Rule(fnet_filter_ext::Rule::test_value()),
    ];
    controller
        .push_changes(resources.iter().cloned().map(fnet_filter_ext::Change::Create).collect())
        .await
        .expect("push changes");
    controller.commit().await.expect("commit pending changes");

    let state =
        realm.connect_to_protocol::<fnet_filter::StateMarker>().expect("connect to protocol");
    let stream = fnet_filter_ext::event_stream_from_state(state).expect("get filter event stream");
    futures::pin_mut!(stream);

    // Observe existing resources and ensure we see what was added.
    let observed: HashMap<_, _> =
        fnet_filter_ext::get_existing_resources(&mut stream).await.expect("get existing");
    assert_eq!(
        observed,
        HashMap::from([(
            controller.id().clone(),
            resources
                .iter()
                .cloned()
                .map(|resource| (resource.id(), resource))
                .collect::<HashMap<_, _>>(),
        )])
    );

    // Drop the controller and ensure that the resources it owned are removed.
    drop(controller);

    let mut resources =
        resources.into_iter().map(|resource| (resource.id(), resource)).collect::<HashMap<_, _>>();
    while !resources.is_empty() {
        let (id, resource) = assert_matches!(
            stream.next().await,
            Some(Ok(fnet_filter_ext::Event::Removed(id, resource))) => (id, resource),
            "resource lifetime should be scoped to controller handle"
        );
        assert_eq!(id, controller_id);
        assert_matches!(resources.remove(&resource), Some(_));
    }
    assert_matches!(
        stream.next().await,
        Some(Ok(fnet_filter_ext::Event::EndOfUpdate)),
        "transactional updates should be demarcated with EndOfUpdate event"
    );
}

#[netstack_test]
async fn push_too_many_changes(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let realm = sandbox.create_netstack_realm::<Netstack3, _>(name).expect("create realm");
    let control =
        realm.connect_to_protocol::<fnet_filter::ControlMarker>().expect("connect to protocol");

    let mut controller = fnet_filter_ext::Controller::new(
        &control,
        &fnet_filter_ext::ControllerId(String::from("test")),
    )
    .await
    .expect("create controller");

    let changes = [
        fnet_filter_ext::Change::Create(fnet_filter_ext::Resource::test_value()),
        fnet_filter_ext::Change::Remove(fnet_filter_ext::ResourceId::test_value()),
    ]
    .into_iter()
    .cycle();

    // Commit a change of the maximum size.
    for batch in &changes
        .clone()
        .take(fnet_filter::MAX_COMMIT_SIZE.into())
        .chunks(fnet_filter::MAX_BATCH_SIZE.into())
    {
        controller.push_changes(batch.collect()).await.expect("push changes");
    }
    controller.commit().await.expect("commit changes");

    // Push one more change than `MAX_COMMIT_SIZE` and ensure we get the
    // expected error.
    for batch in &changes
        .clone()
        .take(fnet_filter::MAX_COMMIT_SIZE.into())
        .chunks(fnet_filter::MAX_BATCH_SIZE.into())
    {
        controller.push_changes(batch.collect()).await.expect("push changes");
    }
    assert_matches!(
        controller.push_changes(changes.take(1).collect()).await,
        Err(fnet_filter_ext::PushChangesError::TooManyChanges)
    );
    // Committing should still succeed because the final change was not pushed
    // to the server.
    controller.commit().await.expect("commit changes");
}

#[netstack_test]
async fn push_commit_zero_changes_is_valid(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let realm = sandbox.create_netstack_realm::<Netstack3, _>(name).expect("create realm");
    let control =
        realm.connect_to_protocol::<fnet_filter::ControlMarker>().expect("connect to protocol");

    let mut controller = fnet_filter_ext::Controller::new(
        &control,
        &fnet_filter_ext::ControllerId(String::from("test")),
    )
    .await
    .expect("create controller");

    controller.push_changes(Vec::new()).await.expect("push zero changes");
    controller.commit().await.expect("commit changes");
}

#[netstack_test]
async fn push_change_missing_required_field(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let realm = sandbox.create_netstack_realm::<Netstack3, _>(name).expect("create realm");
    let control =
        realm.connect_to_protocol::<fnet_filter::ControlMarker>().expect("connect to protocol");

    // Use the FIDL bindings directly rather than going through the extension
    // library, because it intentionally does not allow us to express the
    // invalid types that we are testing.
    let (controller, server_end) = fidl::endpoints::create_proxy().unwrap();
    control.open_controller("test", server_end).expect("open controller");
    let fnet_filter::NamespaceControllerEvent::OnIdAssigned { id: _ } = controller
        .take_event_stream()
        .next()
        .await
        .expect("controller should receive event")
        .expect("controller should be assigned ID");

    assert_eq!(
        controller
            .push_changes(&[fnet_filter::Change::Create(fnet_filter::Resource::Namespace(
                fnet_filter::Namespace {
                    id: None,
                    domain: Some(fnet_filter::Domain::AllIp),
                    ..Default::default()
                }
            ))])
            .await
            .expect("call push changes"),
        fnet_filter::ChangeValidationResult::ErrorOnChange(vec![
            fnet_filter::ChangeValidationError::MissingRequiredField
        ])
    );
}

#[netstack_test]
#[test_case(
    fnet_filter::AddressMatcher {
        matcher: fnet_filter::AddressMatcherType::Range(fnet_filter::AddressRange {
            start: fidl_ip!("192.0.2.1"),
            end: fidl_ip!("2001:db8::1"),
        }),
        invert: false,
    };
    "address family mismatch"
)]
#[test_case(
    fnet_filter::AddressMatcher {
        matcher: fnet_filter::AddressMatcherType::Range(fnet_filter::AddressRange {
            start: fidl_ip!("192.0.2.2"),
            end: fidl_ip!("192.0.2.1"),
        }),
        invert: false,
    };
    "start > end"
)]
#[test_case(
    fnet_filter::AddressMatcher {
        matcher: fnet_filter::AddressMatcherType::Subnet(fnet::Subnet {
            addr: fidl_ip!("192.0.2.1"),
            prefix_len: 33,
        }),
        invert: false,
    };
    "subnet prefix too long"
)]
#[test_case(
    fnet_filter::AddressMatcher {
        matcher: fnet_filter::AddressMatcherType::Subnet(fidl_subnet!("192.0.2.1/24")),
        invert: false,
    };
    "subnet host bits set"
)]
async fn push_change_invalid_address_matcher(name: &str, matcher: fnet_filter::AddressMatcher) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let realm = sandbox.create_netstack_realm::<Netstack3, _>(name).expect("create realm");
    let control =
        realm.connect_to_protocol::<fnet_filter::ControlMarker>().expect("connect to protocol");

    // Use the FIDL bindings directly rather than going through the extension
    // library, because it intentionally does not allow us to express the
    // invalid types that we are testing.
    let (controller, server_end) = fidl::endpoints::create_proxy().unwrap();
    control.open_controller("test", server_end).expect("open controller");
    let fnet_filter::NamespaceControllerEvent::OnIdAssigned { id: _ } = controller
        .take_event_stream()
        .next()
        .await
        .expect("controller should receive event")
        .expect("controller should be assigned ID");

    assert_eq!(
        controller
            .push_changes(&[fnet_filter::Change::Create(fnet_filter::Resource::Rule(
                fnet_filter::Rule {
                    id: fnet_filter::RuleId {
                        routine: fnet_filter::RoutineId {
                            namespace: String::from("namespace"),
                            name: String::from("routine"),
                        },
                        index: 0,
                    },
                    matchers: fnet_filter::Matchers {
                        src_addr: Some(matcher),
                        ..Default::default()
                    },
                    action: fnet_filter::Action::Drop(fnet_filter::Empty {}),
                }
            ))])
            .await
            .expect("call push changes"),
        fnet_filter::ChangeValidationResult::ErrorOnChange(vec![
            fnet_filter::ChangeValidationError::InvalidAddressMatcher
        ])
    );
}

#[netstack_test]
async fn push_change_invalid_port_matcher(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let realm = sandbox.create_netstack_realm::<Netstack3, _>(name).expect("create realm");
    let control =
        realm.connect_to_protocol::<fnet_filter::ControlMarker>().expect("connect to protocol");

    // Use the FIDL bindings directly rather than going through the extension
    // library, because it intentionally does not allow us to express the
    // invalid types that we are testing.
    let (controller, server_end) = fidl::endpoints::create_proxy().unwrap();
    control.open_controller("test", server_end).expect("open controller");
    let fnet_filter::NamespaceControllerEvent::OnIdAssigned { id: _ } = controller
        .take_event_stream()
        .next()
        .await
        .expect("controller should receive event")
        .expect("controller should be assigned ID");

    assert_eq!(
        controller
            .push_changes(&[fnet_filter::Change::Create(fnet_filter::Resource::Rule(
                fnet_filter::Rule {
                    id: fnet_filter::RuleId {
                        routine: fnet_filter::RoutineId {
                            namespace: String::from("namespace"),
                            name: String::from("routine"),
                        },
                        index: 0,
                    },
                    matchers: fnet_filter::Matchers {
                        transport_protocol: Some(fnet_filter::TransportProtocol::Tcp(
                            fnet_filter::TcpMatcher {
                                src_port: Some(fnet_filter::PortMatcher {
                                    start: 1,
                                    end: 0,
                                    invert: false,
                                }),
                                ..Default::default()
                            }
                        )),
                        ..Default::default()
                    },
                    action: fnet_filter::Action::Drop(fnet_filter::Empty {}),
                }
            ))])
            .await
            .expect("call push changes"),
        fnet_filter::ChangeValidationResult::ErrorOnChange(vec![
            fnet_filter::ChangeValidationError::InvalidPortMatcher
        ])
    );
}

enum InvalidChangePosition {
    First,
    Middle,
    Last,
}

#[netstack_test]
#[test_case(InvalidChangePosition::First)]
#[test_case(InvalidChangePosition::Middle)]
#[test_case(InvalidChangePosition::Last)]
async fn push_changes_index_based_error_return(name: &str, pos: InvalidChangePosition) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let realm = sandbox.create_netstack_realm::<Netstack3, _>(name).expect("create realm");
    let control =
        realm.connect_to_protocol::<fnet_filter::ControlMarker>().expect("connect to protocol");

    // Use the FIDL bindings directly rather than going through the extension
    // library, because it intentionally does not allow us to express the
    // invalid types that we are testing.
    let (controller, server_end) = fidl::endpoints::create_proxy().unwrap();
    control.open_controller("test", server_end).expect("open controller");
    let fnet_filter::NamespaceControllerEvent::OnIdAssigned { id: _ } = controller
        .take_event_stream()
        .next()
        .await
        .expect("controller should receive event")
        .expect("controller should be assigned ID");

    // Create a batch of valid changes, and insert an invalid change somewhere in the batch.
    let mut changes =
        vec![fnet_filter::Change::Create(fnet_filter_ext::Resource::test_value().into()); 10];
    let index = match pos {
        InvalidChangePosition::First => 0,
        InvalidChangePosition::Middle => changes.len() / 2,
        InvalidChangePosition::Last => changes.len() - 1,
    };
    changes[index] =
        fnet_filter::Change::Create(fnet_filter::Resource::Namespace(fnet_filter::Namespace {
            id: None,
            domain: Some(fnet_filter::Domain::AllIp),
            ..Default::default()
        }));
    let errors = assert_matches!(
        controller.push_changes(&changes).await.expect("call push changes"),
        fnet_filter::ChangeValidationResult::ErrorOnChange(errors) => errors
    );
    let expected = std::iter::repeat(fnet_filter::ChangeValidationError::Ok)
        .take(index)
        .chain(std::iter::once(fnet_filter::ChangeValidationError::MissingRequiredField))
        .chain(
            std::iter::repeat(fnet_filter::ChangeValidationError::NotReached)
                .take(changes.len() - index - 1),
        )
        .collect::<Vec<_>>();
    assert_eq!(errors, expected);
}

#[netstack_test]
#[test_case(
    vec![
        fnet_filter_ext::Resource::Namespace(fnet_filter_ext::Namespace::test_value()),
        fnet_filter_ext::Resource::Routine(fnet_filter_ext::Routine::test_value()),
        fnet_filter_ext::Resource::Rule(fnet_filter_ext::Rule::test_value()),
    ],
    fnet_filter_ext::ResourceId::Namespace(fnet_filter_ext::NamespaceId::test_value()),
    &[];
    "removing a namespace removes constituent routines and rules"
)]
#[test_case(
    vec![
        fnet_filter_ext::Resource::Namespace(fnet_filter_ext::Namespace::test_value()),
        fnet_filter_ext::Resource::Routine(fnet_filter_ext::Routine::test_value()),
        fnet_filter_ext::Resource::Rule(fnet_filter_ext::Rule::test_value()),
    ],
    fnet_filter_ext::ResourceId::Routine(fnet_filter_ext::RoutineId::test_value()),
    &[fnet_filter_ext::Resource::Namespace(fnet_filter_ext::Namespace::test_value())];
    "removing a routine removes constituent rules"
)]
async fn remove_resource_removes_contents(
    name: &str,
    resources: Vec<fnet_filter_ext::Resource>,
    container: fnet_filter_ext::ResourceId,
    expect_remaining: &[fnet_filter_ext::Resource],
) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let realm = sandbox.create_netstack_realm::<Netstack3, _>(name).expect("create realm");

    let control =
        realm.connect_to_protocol::<fnet_filter::ControlMarker>().expect("connect to protocol");
    let mut controller =
        fnet_filter_ext::Controller::new(&control, &fnet_filter_ext::ControllerId(name.to_owned()))
            .await
            .expect("create controller");

    // Add a resource along with some contents.
    controller
        .push_changes(resources.iter().cloned().map(fnet_filter_ext::Change::Create).collect())
        .await
        .expect("push changes");
    controller.commit().await.expect("commit pending changes");

    let state =
        realm.connect_to_protocol::<fnet_filter::StateMarker>().expect("connect to protocol");
    let stream = fnet_filter_ext::event_stream_from_state(state).expect("get filter event stream");
    futures::pin_mut!(stream);
    let mut observed: HashMap<_, _> =
        fnet_filter_ext::get_existing_resources(&mut stream).await.expect("get resources");
    assert_eq!(
        observed,
        HashMap::from([(
            controller.id().clone(),
            resources
                .into_iter()
                .map(|resource| (resource.id(), resource))
                .collect::<HashMap<_, _>>()
        )])
    );

    // Remove the containing resource.
    controller
        .push_changes(vec![fnet_filter_ext::Change::Remove(container)])
        .await
        .expect("push changes");
    controller.commit().await.expect("commit pending changes");

    fnet_filter_ext::wait_for_condition(&mut stream, &mut observed, |resources| {
        resources
            .get(controller.id())
            .expect("get resources owned by controller")
            .values()
            .eq(expect_remaining.iter())
    })
    .await
    .expect("wait for constituent resources to be removed");
}

#[derive(Clone, Copy)]
enum Idempotence {
    Idempotent,
    NonIdempotent,
}

async fn commit_change_expect_result(
    controller: &mut fnet_filter_ext::Controller,
    change: fnet_filter_ext::Change,
    idempotence: Idempotence,
    expected_result: Result<(), fnet_filter_ext::ChangeCommitError>,
) {
    controller.push_changes(vec![change.clone()]).await.expect("push changes");
    let result = match idempotence {
        Idempotence::Idempotent => controller.commit_idempotent().await,
        Idempotence::NonIdempotent => controller.commit().await,
    };
    match expected_result {
        Ok(()) => result.expect("commit should succeed"),
        Err(expected_error) => {
            let errors = assert_matches!(
                result,
                Err(fnet_filter_ext::CommitError::ErrorOnChange(errors)) => errors
            );
            let (invalid_change, error) = assert_matches!(
                &errors[..],
                [result] => result,
                "should observe one error on commit"
            );
            assert_eq!(invalid_change, &change);
            assert_eq!(error, &expected_error);
        }
    }
}

#[netstack_test]
#[test_case(
    fnet_filter_ext::Resource::Namespace(fnet_filter_ext::Namespace::test_value()),
    fnet_filter_ext::Resource::Namespace(
        fnet_filter_ext::Namespace {
            domain: fnet_filter_ext::Domain::Ipv6,
            ..fnet_filter_ext::Namespace::test_value()
        },
    );
    "namespace"
)]
#[test_case(
    fnet_filter_ext::Resource::Routine(fnet_filter_ext::Routine::test_value()),
    fnet_filter_ext::Resource::Routine(
        fnet_filter_ext::Routine {
            routine_type: fnet_filter_ext::RoutineType::Nat(None),
            ..fnet_filter_ext::Routine::test_value()
        },
    );
    "routine"
)]
#[test_case(
    fnet_filter_ext::Resource::Rule(fnet_filter_ext::Rule::test_value()),
    fnet_filter_ext::Resource::Rule(
        fnet_filter_ext::Rule {
            matchers: fnet_filter_ext::Matchers::default(),
            ..fnet_filter_ext::Rule::test_value()
        },
    );
    "rule"
)]
async fn add_existing_resource_idempotent(
    name: &str,
    duplicate_resource: fnet_filter_ext::Resource,
    resource_with_same_id_different_property: fnet_filter_ext::Resource,
) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let realm = sandbox.create_netstack_realm::<Netstack3, _>(name).expect("create realm");
    let control =
        realm.connect_to_protocol::<fnet_filter::ControlMarker>().expect("connect to protocol");

    let mut controller =
        fnet_filter_ext::Controller::new(&control, &fnet_filter_ext::ControllerId(name.to_owned()))
            .await
            .expect("create controller");
    let resources = [
        fnet_filter_ext::Resource::Namespace(fnet_filter_ext::Namespace::test_value()),
        fnet_filter_ext::Resource::Routine(fnet_filter_ext::Routine::test_value()),
        fnet_filter_ext::Resource::Rule(fnet_filter_ext::Rule::test_value()),
    ];
    controller
        .push_changes(resources.iter().cloned().map(fnet_filter_ext::Change::Create).collect())
        .await
        .expect("push changes");
    controller.commit().await.expect("commit pending changes");

    let state =
        realm.connect_to_protocol::<fnet_filter::StateMarker>().expect("connect to protocol");
    let stream = fnet_filter_ext::event_stream_from_state(state).expect("get filter event stream");
    futures::pin_mut!(stream);
    let observed: HashMap<_, _> =
        fnet_filter_ext::get_existing_resources(&mut stream).await.expect("get resources");
    assert_eq!(
        observed,
        HashMap::from([(
            controller.id().clone(),
            resources
                .into_iter()
                .map(|resource| (resource.id(), resource))
                .collect::<HashMap<_, _>>(),
        )])
    );

    // Add a resource with the same ID as the existing one, but with one of its
    // properties changed. This should always fail, even if `idempotent` is set.
    commit_change_expect_result(
        &mut controller,
        fnet_filter_ext::Change::Create(resource_with_same_id_different_property.clone()),
        Idempotence::Idempotent,
        Err(fnet_filter_ext::ChangeCommitError::AlreadyExists),
    )
    .await;
    commit_change_expect_result(
        &mut controller,
        fnet_filter_ext::Change::Create(resource_with_same_id_different_property),
        Idempotence::NonIdempotent,
        Err(fnet_filter_ext::ChangeCommitError::AlreadyExists),
    )
    .await;

    // Add a resource that exactly matches the existing one. If the commit is
    // idempotent, this should succeed; if not, it should fail.
    commit_change_expect_result(
        &mut controller,
        fnet_filter_ext::Change::Create(duplicate_resource.clone()),
        Idempotence::Idempotent,
        Ok(()),
    )
    .await;
    commit_change_expect_result(
        &mut controller,
        fnet_filter_ext::Change::Create(duplicate_resource),
        Idempotence::NonIdempotent,
        Err(fnet_filter_ext::ChangeCommitError::AlreadyExists),
    )
    .await;

    // Whether the attempted changes fail, or succeed idempotently, either way
    // the watcher should observe no changes. Contained resources should also
    // remain installed (routines for a namespace and rules for a routine).
    assert_matches!(
        stream.next().on_timeout(ASYNC_EVENT_NEGATIVE_CHECK_TIMEOUT.after_now(), || None).await,
        None,
        "no changes should be observed"
    );
}

#[netstack_test]
#[test_case(
    fnet_filter_ext::ResourceId::Namespace(fnet_filter_ext::NamespaceId::test_value());
    "namespace"
)]
#[test_case(
    fnet_filter_ext::ResourceId::Routine(fnet_filter_ext::RoutineId::test_value());
    "routine"
)]
#[test_case(fnet_filter_ext::ResourceId::Rule(fnet_filter_ext::RuleId::test_value()); "rule")]
async fn remove_unknown_resource_idempotent(name: &str, resource: fnet_filter_ext::ResourceId) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let realm = sandbox.create_netstack_realm::<Netstack3, _>(name).expect("create realm");
    let control =
        realm.connect_to_protocol::<fnet_filter::ControlMarker>().expect("connect to protocol");
    let mut controller =
        fnet_filter_ext::Controller::new(&control, &fnet_filter_ext::ControllerId(name.to_owned()))
            .await
            .expect("create controller");

    let state =
        realm.connect_to_protocol::<fnet_filter::StateMarker>().expect("connect to protocol");
    let stream = fnet_filter_ext::event_stream_from_state(state).expect("get filter event stream");
    futures::pin_mut!(stream);
    let observed: HashMap<_, _> =
        fnet_filter_ext::get_existing_resources(&mut stream).await.expect("get resources");
    assert!(observed.is_empty());

    // Remove a resource that doesn't exist. If the commit is idempotent, this
    // should succeed; if not, it should fail.
    commit_change_expect_result(
        &mut controller,
        fnet_filter_ext::Change::Remove(resource.clone()),
        Idempotence::Idempotent,
        Ok(()),
    )
    .await;
    commit_change_expect_result(
        &mut controller,
        fnet_filter_ext::Change::Remove(resource),
        Idempotence::NonIdempotent,
        Err(fnet_filter_ext::ChangeCommitError::NamespaceNotFound),
    )
    .await;

    // Whether the attempted change fails or succeeds, either way the watcher
    // should observe no actual changes.
    assert_matches!(
        stream.next().on_timeout(ASYNC_EVENT_NEGATIVE_CHECK_TIMEOUT.after_now(), || None).await,
        None,
        "no changes should be observed"
    );
}

#[netstack_test]
#[test_case(Idempotence::Idempotent)]
#[test_case(Idempotence::NonIdempotent)]
async fn reference_unknown_resource(name: &str, idempotence: Idempotence) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let realm = sandbox.create_netstack_realm::<Netstack3, _>(name).expect("create realm");
    let control =
        realm.connect_to_protocol::<fnet_filter::ControlMarker>().expect("connect to protocol");
    let mut controller =
        fnet_filter_ext::Controller::new(&control, &fnet_filter_ext::ControllerId(name.to_owned()))
            .await
            .expect("create controller");

    // Adding a routine that refers to an unknown namespace should fail.
    commit_change_expect_result(
        &mut controller,
        fnet_filter_ext::Change::Create(fnet_filter_ext::Resource::Routine(
            fnet_filter_ext::Routine::test_value(),
        )),
        idempotence,
        Err(fnet_filter_ext::ChangeCommitError::NamespaceNotFound),
    )
    .await;

    // Adding a rule that refers to an unknown namespace should fail.
    commit_change_expect_result(
        &mut controller,
        fnet_filter_ext::Change::Create(fnet_filter_ext::Resource::Rule(
            fnet_filter_ext::Rule::test_value(),
        )),
        idempotence,
        Err(fnet_filter_ext::ChangeCommitError::NamespaceNotFound),
    )
    .await;

    // Add the namespace the rule is referring to. The rule creation should
    // still fail because it refers to an unknown routine.
    controller
        .push_changes(vec![fnet_filter_ext::Change::Create(fnet_filter_ext::Resource::Namespace(
            fnet_filter_ext::Namespace::test_value(),
        ))])
        .await
        .expect("push change");
    controller.commit().await.expect("commit pending change");
    commit_change_expect_result(
        &mut controller,
        fnet_filter_ext::Change::Create(fnet_filter_ext::Resource::Rule(
            fnet_filter_ext::Rule::test_value(),
        )),
        idempotence,
        Err(fnet_filter_ext::ChangeCommitError::RoutineNotFound),
    )
    .await;

    // Add the routine the rule is referring to. The rule creation should still
    // fail because it contains a `Jump` action that refers to an unknown
    // routine.
    controller
        .push_changes(vec![fnet_filter_ext::Change::Create(fnet_filter_ext::Resource::Routine(
            fnet_filter_ext::Routine::test_value(),
        ))])
        .await
        .expect("push change");
    commit_change_expect_result(
        &mut controller,
        fnet_filter_ext::Change::Create(fnet_filter_ext::Resource::Rule(fnet_filter_ext::Rule {
            action: fnet_filter_ext::Action::Jump(String::from("does-not-exist")),
            ..fnet_filter_ext::Rule::test_value()
        })),
        idempotence,
        Err(fnet_filter_ext::ChangeCommitError::RoutineNotFound),
    )
    .await;
}

#[netstack_test]
async fn commit_failure_clears_pending_changes_and_does_not_change_state(name: &str) {
    let sandbox = netemul::TestSandbox::new().expect("create sandbox");
    let realm = sandbox.create_netstack_realm::<Netstack3, _>(name).expect("create realm");
    let control =
        realm.connect_to_protocol::<fnet_filter::ControlMarker>().expect("connect to protocol");
    let mut controller =
        fnet_filter_ext::Controller::new(&control, &fnet_filter_ext::ControllerId(name.to_owned()))
            .await
            .expect("create controller");

    // Add some state and observe its addition.
    let resources = [
        fnet_filter_ext::Resource::Namespace(fnet_filter_ext::Namespace::test_value()),
        fnet_filter_ext::Resource::Routine(fnet_filter_ext::Routine::test_value()),
        fnet_filter_ext::Resource::Rule(fnet_filter_ext::Rule::test_value()),
    ];
    controller
        .push_changes(resources.iter().cloned().map(fnet_filter_ext::Change::Create).collect())
        .await
        .expect("push changes");
    controller.commit().await.expect("commit pending changes");

    let state =
        realm.connect_to_protocol::<fnet_filter::StateMarker>().expect("connect to protocol");
    let stream = fnet_filter_ext::event_stream_from_state(state).expect("get filter event stream");
    futures::pin_mut!(stream);
    let observed: HashMap<_, _> =
        fnet_filter_ext::get_existing_resources(&mut stream).await.expect("get resources");
    assert_eq!(
        observed,
        HashMap::from([(
            controller.id().clone(),
            resources
                .into_iter()
                .map(|resource| (resource.id(), resource))
                .collect::<HashMap<_, _>>(),
        )])
    );

    // Commit an invalid change and ensure we get an error.
    commit_change_expect_result(
        &mut controller,
        fnet_filter_ext::Change::Remove(fnet_filter_ext::ResourceId::Namespace(
            fnet_filter_ext::NamespaceId(String::from("does-not-exist")),
        )),
        Idempotence::NonIdempotent,
        Err(fnet_filter_ext::ChangeCommitError::NamespaceNotFound),
    )
    .await;

    // Committing again should succeed because the pending change should have
    // been cleared on commit failure.
    controller.commit().await.expect("commit with no pending changes should succeed");

    // State should be unchanged, so the watcher should not observe any events.
    assert_matches!(
        stream.next().on_timeout(ASYNC_EVENT_NEGATIVE_CHECK_TIMEOUT.after_now(), || None).await,
        None,
        "no changes should be observed"
    );
}
