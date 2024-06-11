// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use futures::stream::{BoxStream, SelectAll};
use futures::{FutureExt, Stream, StreamExt};
use pin_project::pin_project;
use std::collections::HashMap;
use std::pin::Pin;
use std::task::{Context, Poll};
use zx::AsHandleRef;
use {
    fidl_fuchsia_component as fcomponent, fidl_fuchsia_memory_attribution as fattribution,
    fuchsia_zircon as zx,
};

/// A simple tree breakdown of resource usage useful for tests.
#[derive(Debug, Clone)]
pub struct Principal {
    /// Name of the principal.
    pub name: String,

    /// Resources used by this principal.
    pub resources: Vec<zx::Koid>,

    /// Children of the principal.
    pub children: Vec<Principal>,
}

impl Principal {
    pub fn new(identifier: String) -> Principal {
        Principal { name: identifier, resources: vec![], children: vec![] }
    }
}

/// Obtain which resources are used for various activities by an attribution provider.
///
/// If one of the children under the attribution provider has detailed attribution
/// information, this function will recursively visit those children and build a
/// tree of nodes.
///
/// Returns a stream of tree that are momentary snapshots of the memory state.
/// The tree will evolve over time as principals are added and removed.
pub fn attribute_memory(
    name: String,
    attribution_provider: fattribution::ProviderProxy,
    introspector: fcomponent::IntrospectorProxy,
) -> BoxStream<'static, Principal> {
    futures::stream::unfold(StreamState::new(name, introspector, attribution_provider), get_next)
        .boxed()
}

/// Wait for the next hanging-get message and recompute the tree.
async fn get_next(mut state: StreamState) -> Option<(Principal, StreamState)> {
    let mut node = state.node.clone().unwrap_or_else(|| Principal::new(state.name.clone()));
    let mut children: HashMap<String, Principal> =
        node.children.clone().into_iter().map(|n| (n.name.clone(), n)).collect();

    // Wait for new attribution information.
    match state.next().await {
        Some(event) => {
            match event {
                // New attribution information for this principal.
                Event::Node(attributions) => {
                    for attribution in attributions {
                        handle_update(attribution, &node, &mut state, &mut children).await;
                    }
                }
                // New attribution information for a child principal.
                Event::Child(child) => {
                    children.insert(child.name.clone(), child);
                }
            }
        }
        None => return None,
    }

    node.children = children.into_values().collect();
    state.node = Some(node.clone());
    Some((node, state))
}

async fn handle_update(
    attribution: fattribution::AttributionUpdate,
    node: &Principal,
    state: &mut StreamState,
    children: &mut HashMap<String, Principal>,
) {
    match attribution {
        fattribution::AttributionUpdate::Add(new_principal) => {
            let identifier = get_identifier_string(
                new_principal.identifier.unwrap(),
                &node.name,
                &state.introspector,
                &mut state.koid_to_component_moniker,
            )
            .await;

            // Recursively attribute memory in this child principal if applicable.
            if let Some(client) = new_principal.detailed_attribution {
                state.child_update.push(
                    attribute_memory(
                        identifier.clone(),
                        client.into_proxy().unwrap(),
                        state.introspector.clone(),
                    )
                    .boxed(),
                );
            }
            children.insert(identifier.clone(), Principal::new(identifier));
        }
        fattribution::AttributionUpdate::Update(updated_principal) => {
            let identifier = get_identifier_string(
                updated_principal.identifier.unwrap(),
                &node.name,
                &state.introspector,
                &mut state.koid_to_component_moniker,
            )
            .await;

            let child = children.get_mut(&identifier).unwrap();
            match updated_principal.resources.unwrap() {
                fattribution::Resources::Data(d) => {
                    child.resources = d
                        .into_iter()
                        .filter_map(|r| {
                            if let fattribution::Resource::KernelObject(koid) = r {
                                Some(zx::Koid::from_raw(koid))
                            } else {
                                None
                            }
                        })
                        .collect();
                }
                _ => todo!("unimplemented"),
            };
        }
        fattribution::AttributionUpdate::Remove(identifier) => {
            let name = get_identifier_string(
                identifier,
                &node.name,
                &state.introspector,
                &mut state.koid_to_component_moniker,
            )
            .await;
            children.remove(&name);
        }
        x @ _ => panic!("unimplemented {x:?}"),
    }
}

async fn get_identifier_string(
    identifier: fattribution::Identifier,
    name: &String,
    introspector: &fcomponent::IntrospectorProxy,
    koid_to_component_moniker: &mut HashMap<zx::Koid, String>,
) -> String {
    match identifier {
        fattribution::Identifier::Self_(_) => todo!("self attribution not supported"),
        fattribution::Identifier::Component(c) => {
            let koid = c.get_koid().unwrap();
            if let Some(moniker) = koid_to_component_moniker.get(&koid) {
                moniker.to_owned()
            } else {
                let moniker = introspector
                    .get_moniker(c)
                    .await
                    .expect("Inspector call failed")
                    .expect("Inspector::GetMoniker call failed");
                koid_to_component_moniker.insert(koid, moniker.clone());
                moniker
            }
        }
        fattribution::Identifier::Part(sc) => format!("{}/{}", name, sc).to_owned(),
        _ => todo!(),
    }
}

/// [`StreamState`] holds attribution information for a given tree of principals
/// rooted at the one identified by `name`.
///
/// It implements a [`Stream`] and will yield the next update to the tree when
/// any of the hanging-gets from principals in this tree returns.
///
/// [`get_next`] will poll this stream to process the update, such as adding a new
/// child principal.
#[pin_project]
struct StreamState {
    /// The name of the principal at the root of the tree.
    name: String,

    /// A capability used to unseal component instance tokens back to monikers.
    introspector: fcomponent::IntrospectorProxy,

    /// A cached mapping from component instance tokens KOIDs to component monikers.
    koid_to_component_moniker: HashMap<zx::Koid, String>,

    /// The tree of principals rooted at `node`.
    node: Option<Principal>,

    /// A stream of `AttributionUpdate` events for the current principal.
    hanging_get_update: BoxStream<'static, Vec<fattribution::AttributionUpdate>>,

    /// A stream of child principal updates. Each `Principal` element should
    /// replace the existing child principal if there already is a child with
    /// the same name. [`SelectAll`] is used to merge the updates from all children
    /// into a single stream.
    #[pin]
    child_update: SelectAll<BoxStream<'static, Principal>>,
}

impl StreamState {
    fn new(
        name: String,
        introspector: fcomponent::IntrospectorProxy,
        attribution_provider: fattribution::ProviderProxy,
    ) -> Self {
        Self {
            name: name.clone(),
            introspector,
            koid_to_component_moniker: HashMap::new(),
            node: None,
            hanging_get_update: Box::pin(hanging_get_stream(name, attribution_provider)),
            child_update: SelectAll::new(),
        }
    }
}

fn hanging_get_stream(
    name: String,
    proxy: fattribution::ProviderProxy,
) -> impl Stream<Item = Vec<fattribution::AttributionUpdate>> + 'static {
    futures::stream::unfold(proxy, move |proxy| {
        let name = name.clone();
        proxy.get().map(move |get_result| {
            let attributions = get_result
                .unwrap_or_else(|e| panic!("Failed to get AttributionResponse for {name}: {e}"))
                .unwrap_or_else(|e| panic!("Failed call to AttributionResponse for {name}: {e:?}"))
                .attributions
                .unwrap_or_else(|| panic!("Failed memory attribution for {name}"));
            Some((attributions, proxy))
        })
    })
}

enum Event {
    Node(Vec<fattribution::AttributionUpdate>),
    Child(Principal),
}

impl Stream for StreamState {
    type Item = Event;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();
        match this.child_update.poll_next_unpin(cx) {
            Poll::Ready(Some(node)) => {
                return Poll::Ready(Some(Event::Child(node)));
            }
            Poll::Ready(None) => {}
            Poll::Pending => {}
        }
        match this.hanging_get_update.poll_next_unpin(cx) {
            Poll::Ready(attributions) => {
                return Poll::Ready(Some(Event::Node(attributions.unwrap())));
            }
            Poll::Pending => {}
        }
        return Poll::Pending;
    }
}
