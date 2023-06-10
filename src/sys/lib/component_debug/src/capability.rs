// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::realm::{
        get_all_instances, get_resolved_declaration, GetAllInstancesError, GetDeclarationError,
    },
    cm_rust::{ComponentDecl, SourceName},
    fidl_fuchsia_sys2 as fsys,
    moniker::AbsoluteMoniker,
    thiserror::Error,
};

// Export so it is easier for others to use.
pub use routing::mapper::RouteSegment;

#[derive(Debug, Error)]
pub enum FindInstancesError {
    #[error("failed to get all instances: {0}")]
    GetAllInstancesError(#[from] GetAllInstancesError),

    #[error("failed to get manifest for {moniker}: {err}")]
    GetDeclarationError {
        moniker: AbsoluteMoniker,
        #[source]
        err: GetDeclarationError,
    },
}

/// Find components that reference a capability matching the given |query|.
pub async fn get_all_route_segments(
    query: String,
    realm_query: &fsys::RealmQueryProxy,
) -> Result<Vec<RouteSegment>, FindInstancesError> {
    let instances = get_all_instances(realm_query).await?;
    let mut segments = vec![];

    for instance in instances {
        match get_resolved_declaration(&instance.moniker, realm_query).await {
            Ok(decl) => {
                let mut component_segments = get_segments(&instance.moniker, decl, &query);
                segments.append(&mut component_segments)
            }
            Err(GetDeclarationError::InstanceNotResolved(_)) => continue,
            Err(err) => {
                return Err(FindInstancesError::GetDeclarationError {
                    moniker: instance.moniker.clone(),
                    err,
                })
            }
        }
    }

    Ok(segments)
}

/// Determine if a capability matching the |query| is declared, exposed, used or offered by
/// this component.
fn get_segments(
    moniker: &AbsoluteMoniker,
    manifest: ComponentDecl,
    query: &str,
) -> Vec<RouteSegment> {
    let mut segments = vec![];

    for capability in manifest.capabilities {
        if capability.name().to_string().contains(query) {
            segments.push(RouteSegment::DeclareBy { moniker: moniker.clone(), capability });
        }
    }

    for expose in manifest.exposes {
        if expose.source_name().to_string().contains(query) {
            segments.push(RouteSegment::ExposeBy { moniker: moniker.clone(), capability: expose });
        }
    }

    for use_ in manifest.uses {
        if use_.source_name().to_string().contains(query) {
            segments.push(RouteSegment::UseBy { moniker: moniker.clone(), capability: use_ });
        }
    }

    for offer in manifest.offers {
        if offer.source_name().to_string().contains(query) {
            segments.push(RouteSegment::OfferBy { moniker: moniker.clone(), capability: offer });
        }
    }

    segments
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::*;
    use cm_rust::*;
    use fidl_fuchsia_component_decl as fdecl;
    use std::collections::HashMap;

    fn create_realm_query() -> fsys::RealmQueryProxy {
        serve_realm_query(
            vec![fsys::Instance {
                moniker: Some("./my_foo".to_string()),
                url: Some("fuchsia-pkg://fuchsia.com/foo#meta/foo.cm".to_string()),
                instance_id: None,
                resolved_info: Some(fsys::ResolvedInfo {
                    resolved_url: Some("fuchsia-pkg://fuchsia.com/foo#meta/foo.cm".to_string()),
                    execution_info: None,
                    ..Default::default()
                }),
                ..Default::default()
            }],
            HashMap::from([(
                "./my_foo".to_string(),
                ComponentDecl {
                    children: vec![ChildDecl {
                        name: "my_bar".to_string(),
                        url: "fuchsia-pkg://fuchsia.com/bar#meta/bar.cm".to_string(),
                        startup: fdecl::StartupMode::Lazy,
                        environment: None,
                        config_overrides: None,
                        on_terminate: None,
                    }],
                    uses: vec![UseDecl::Protocol(UseProtocolDecl {
                        source: UseSource::Parent,
                        source_name: "fuchsia.foo.bar".parse().unwrap(),
                        target_path: "/svc/fuchsia.foo.bar".parse().unwrap(),
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    })],
                    exposes: vec![ExposeDecl::Protocol(ExposeProtocolDecl {
                        source: ExposeSource::Self_,
                        source_name: "fuchsia.foo.bar".parse().unwrap(),
                        target: ExposeTarget::Parent,
                        target_name: "fuchsia.foo.bar".parse().unwrap(),
                        availability: Availability::Required,
                    })],
                    offers: vec![OfferDecl::Protocol(OfferProtocolDecl {
                        source: OfferSource::Self_,
                        source_name: "fuchsia.foo.bar".parse().unwrap(),
                        target: OfferTarget::Child(ChildRef {
                            name: "my_bar".into(),
                            collection: None,
                        }),
                        target_name: "fuchsia.foo.bar".parse().unwrap(),
                        dependency_type: DependencyType::Strong,
                        availability: Availability::Required,
                    })],
                    capabilities: vec![CapabilityDecl::Protocol(ProtocolDecl {
                        name: "fuchsia.foo.bar".parse().unwrap(),
                        source_path: Some("/svc/fuchsia.foo.bar".parse().unwrap()),
                    })],
                    ..ComponentDecl::default()
                }
                .native_into_fidl(),
            )]),
            HashMap::new(),
            HashMap::new(),
        )
    }

    #[fuchsia::test]
    async fn segments() {
        let realm_query = create_realm_query();

        let segments =
            get_all_route_segments("fuchsia.foo.bar".to_string(), &realm_query).await.unwrap();

        assert_eq!(segments.len(), 4);

        let mut found_use = false;
        let mut found_offer = false;
        let mut found_expose = false;
        let mut found_declaration = false;

        for segment in segments {
            match segment {
                RouteSegment::UseBy { moniker, capability } => {
                    found_use = true;
                    assert_eq!(moniker, "/my_foo".try_into().unwrap());
                    assert_eq!(
                        capability,
                        UseDecl::Protocol(UseProtocolDecl {
                            source: UseSource::Parent,
                            source_name: "fuchsia.foo.bar".parse().unwrap(),
                            target_path: "/svc/fuchsia.foo.bar".parse().unwrap(),
                            dependency_type: DependencyType::Strong,
                            availability: Availability::Required
                        })
                    );
                }
                RouteSegment::OfferBy { moniker, capability } => {
                    found_offer = true;
                    assert_eq!(moniker, "/my_foo".try_into().unwrap());
                    assert_eq!(
                        capability,
                        OfferDecl::Protocol(OfferProtocolDecl {
                            source: OfferSource::Self_,
                            source_name: "fuchsia.foo.bar".parse().unwrap(),
                            target: OfferTarget::Child(ChildRef {
                                name: "my_bar".into(),
                                collection: None,
                            }),
                            target_name: "fuchsia.foo.bar".parse().unwrap(),
                            dependency_type: DependencyType::Strong,
                            availability: Availability::Required
                        })
                    );
                }
                RouteSegment::ExposeBy { moniker, capability } => {
                    found_expose = true;
                    assert_eq!(moniker, "/my_foo".try_into().unwrap());
                    assert_eq!(
                        capability,
                        ExposeDecl::Protocol(ExposeProtocolDecl {
                            source: ExposeSource::Self_,
                            source_name: "fuchsia.foo.bar".parse().unwrap(),
                            target: ExposeTarget::Parent,
                            target_name: "fuchsia.foo.bar".parse().unwrap(),
                            availability: Availability::Required
                        })
                    );
                }
                RouteSegment::DeclareBy { moniker, capability } => {
                    found_declaration = true;
                    assert_eq!(moniker, "/my_foo".try_into().unwrap());
                    assert_eq!(
                        capability,
                        CapabilityDecl::Protocol(ProtocolDecl {
                            name: "fuchsia.foo.bar".parse().unwrap(),
                            source_path: Some("/svc/fuchsia.foo.bar".parse().unwrap()),
                        })
                    );
                }
                _ => panic!("unexpected segment"),
            }
        }

        assert!(found_use);
        assert!(found_expose);
        assert!(found_offer);
        assert!(found_declaration);
    }
}
