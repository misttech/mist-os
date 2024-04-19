// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#![cfg(test)]

use fidl_fuchsia_component as fcomponent;
use fidl_fuchsia_component_decl as fdecl;
use fidl_fuchsia_examples_colocated as fcolocated;
use fidl_fuchsia_memory_attribution as fattribution;
use fidl_fuchsia_process::HandleInfo;
use fuchsia_async as fasync;
use fuchsia_component_test::{
    Capability, ChildOptions, RealmBuilder, RealmBuilderParams, Ref, Route,
};
use fuchsia_runtime::HandleType;
use fuchsia_zircon as zx;
use futures_util::{future::BoxFuture, FutureExt};
use std::{collections::HashMap, sync::Arc};

/// Attribute resource given the set of resources at the root of the system,
/// and a protocol to attribute resources under different principals.
fn attribute_memory<'a>(
    name: String,
    attribution_provider: fattribution::ProviderProxy,
    introspector: &'a fcomponent::IntrospectorProxy,
) -> BoxFuture<'a, Node> {
    async move {
        // Otherwise, check if there are attribution information.
        let attributions = attribution_provider
            .get()
            .await
            .unwrap_or_else(|e| panic!("Failed to get AttributionResponse for {name}: {e}"))
            .unwrap_or_else(|e| panic!("Failed call to AttributionResponse for {name}: {e:?}"))
            .attributions
            .unwrap_or_else(|| panic!("Failed memory attribution for {name}"));

        let mut node = Node::new(name);

        // If there are children, resources assigned to this node by its parent
        // will be re-assigned to children if applicable.
        let mut children = HashMap::<String, Node>::new();
        for attribution in attributions {
            // Recursively attribute memory in this child principal.
            match attribution {
                fattribution::AttributionUpdate::Add(new_principal) => {
                    let identifier =
                        get_identifier_string(new_principal.identifier, &node.name, introspector)
                            .await;
                    let child = if let Some(client) = new_principal.detailed_attribution {
                        attribute_memory(identifier, client.into_proxy().unwrap(), introspector)
                            .await
                    } else {
                        Node::new(identifier)
                    };
                    children.insert(child.name.clone(), child);
                }
                fattribution::AttributionUpdate::Update(updated_principal) => {
                    let identifier = get_identifier_string(
                        updated_principal.identifier,
                        &node.name,
                        introspector,
                    )
                    .await;

                    let child = children.get_mut(&identifier).unwrap();
                    match updated_principal.resources.unwrap() {
                        fattribution::Resources::Data(d) => {
                            child.resources = d
                                .into_iter()
                                .filter_map(|r| {
                                    if let fattribution::Resource::KernelObject(koid) = r {
                                        Some(koid)
                                    } else {
                                        None
                                    }
                                })
                                .collect();
                        }
                        _ => todo!("unimplemented"),
                    };
                }
                fattribution::AttributionUpdate::Remove(_) => todo!(),
                _ => panic!("Unimplemented"),
            };
        }
        node.children.extend(children.into_values());
        node
    }
    .boxed()
}

async fn get_identifier_string(
    identifier: Option<fattribution::Identifier>,
    name: &String,
    introspector: &fcomponent::IntrospectorProxy,
) -> String {
    match identifier.unwrap() {
        fattribution::Identifier::Self_(_) => todo!("self attribution not supported"),
        fattribution::Identifier::Component(c) => introspector
            .get_moniker(c)
            .await
            .expect("Inspector call failed")
            .expect("Inspector::GetMoniker call failed"),
        fattribution::Identifier::Part(sc) => format!("{}/{}", name, sc).to_owned(),
        _ => todo!(),
    }
}

#[derive(Debug)]
struct Node {
    name: String,
    resources: Vec<u64>,
    children: Vec<Node>,
}

impl Node {
    pub fn new(identifier: String) -> Node {
        Node { name: identifier, resources: vec![], children: vec![] }
    }
}

#[fuchsia::test]
async fn test_attribute_memory() {
    // Starts a component manager and obtain its root job, so that we can simulate
    // traversing the root job of the system, the kind done in `memory_monitor`.
    let builder = RealmBuilder::with_params(
        RealmBuilderParams::new().from_relative_url("#meta/test_realm.cm"),
    )
    .await
    .expect("Failed to create test realm builder");

    // Add a child to receive these capabilities so that we can use them in this test.
    // - fuchsia.memory.attribution.Provider
    // - fuchsia.kernel.RootJobForInspect
    // - fuchsia.component.Realm
    struct Capabilities {
        attribution_provider: fattribution::ProviderProxy,
        introspector: fcomponent::IntrospectorProxy,
        realm: fcomponent::RealmProxy,
    }
    let (capabilities_sender, capabilities_receiver) = async_channel::unbounded::<Capabilities>();
    let capabilities_sender = Arc::new(capabilities_sender);
    let receiver = builder
        .add_local_child(
            "receiver",
            move |handles| {
                let capabilities_sender = capabilities_sender.clone();
                async move {
                    capabilities_sender
                        .send(Capabilities {
                            attribution_provider: handles
                                .connect_to_protocol::<fattribution::ProviderMarker>()
                                .unwrap(),
                            introspector: handles
                                .connect_to_protocol::<fcomponent::IntrospectorMarker>()
                                .unwrap(),
                            realm: handles
                                .connect_to_protocol::<fcomponent::RealmMarker>()
                                .unwrap(),
                        })
                        .await
                        .unwrap();
                    // TODO(https://fxbug.dev/303919602): Until the component framework reliably
                    // drains capability requests when a component is stopped, we need to
                    // keep running the component.
                    std::future::pending().await
                }
                .boxed()
            },
            ChildOptions::new().eager(),
        )
        .await
        .expect("Failed to add child");

    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol_by_name("fuchsia.memory.attribution.Provider"))
                .from(Ref::child("elf_runner"))
                .to(&receiver),
        )
        .await
        .unwrap();

    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol_by_name("fuchsia.component.Realm"))
                .from(Ref::framework())
                .to(&receiver),
        )
        .await
        .unwrap();

    builder
        .add_route(
            Route::new()
                .capability(Capability::protocol_by_name("fuchsia.component.Introspector"))
                .from(Ref::framework())
                .to(&receiver),
        )
        .await
        .unwrap();

    let _realm =
        builder.build_in_nested_component_manager("#meta/component_manager.cm").await.unwrap();

    let capabilities = capabilities_receiver.recv().await.unwrap();

    // Start a colocated component.
    let collection = fdecl::CollectionRef { name: "collection".to_string() };
    let decl = fdecl::Child {
        name: Some("colocated-component".to_string()),
        url: Some("#meta/colocated-component.cm".to_string()),
        startup: Some(fdecl::StartupMode::Lazy),
        ..Default::default()
    };
    let (user0, user0_peer) = zx::Channel::create();
    let args = fcomponent::CreateChildArgs {
        numbered_handles: Some(vec![HandleInfo {
            handle: user0_peer.into(),
            id: fuchsia_runtime::HandleInfo::new(HandleType::User0, 0).as_raw(),
        }]),
        ..Default::default()
    };
    capabilities.realm.create_child(&collection, &decl, args).await.unwrap().unwrap();

    let colocated_component_vmos =
        fcolocated::ColocatedProxy::new(fasync::Channel::from_channel(user0))
            .get_vmos()
            .await
            .unwrap();

    assert!(!colocated_component_vmos.is_empty());

    // Starting from the ELF runner, ask about the resource usage.
    let elf_runner = attribute_memory(
        "elf_runner.cm".to_owned(),
        capabilities.attribution_provider,
        &capabilities.introspector,
    )
    .await;

    // We should get the following tree:
    //
    // - elf_runner.cm
    //     - colocated_runner.cm
    //         - colocated_component-64mb.cm
    //             - Some VMO
    //         - overhead
    //     - overhead
    eprintln!("{:?}", elf_runner);
    assert_eq!(elf_runner.children.len(), 1);
    assert!(elf_runner.children[0].name.contains("colocated-runner"));
    assert_eq!(elf_runner.children[0].children.len(), 1usize);
    // Name of the colocated component
    assert_eq!(&elf_runner.children[0].children[0].name, "collection:colocated-component");
    let resource = &elf_runner.children[0].children[0].resources;
    for vmo_koid in colocated_component_vmos {
        assert!(resource.contains(&vmo_koid));
    }
}
