// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::realm::{get_all_instances, get_manifest, GetAllInstancesError, GetManifestError},
    cm_rust::{ComponentDecl, ExposeDecl, UseDecl},
    fidl_fuchsia_sys2 as fsys,
    moniker::AbsoluteMoniker,
    thiserror::Error,
};

#[derive(Debug, Error)]
pub enum FindInstancesError {
    #[error("failed to get all instances: {0}")]
    GetAllInstancesError(#[from] GetAllInstancesError),

    #[error("failed to get manifest for {moniker}: {err}")]
    GetManifestError {
        moniker: AbsoluteMoniker,
        #[source]
        err: GetManifestError,
    },
}

/// Component instances that use/expose a given capability, separated into two vectors (one for
/// components that expose the capability, the other for components that use the capability).
pub struct MatchingInstances {
    pub exposed: Vec<AbsoluteMoniker>,
    pub used: Vec<AbsoluteMoniker>,
}

/// Find components that expose or use a given capability. The capability must be a protocol name or
/// a directory capability name.
pub async fn find_instances_that_expose_or_use_capability(
    capability: String,
    realm_query: &fsys::RealmQueryProxy,
) -> Result<MatchingInstances, FindInstancesError> {
    let instances = get_all_instances(realm_query).await?;
    let mut matching_instances = MatchingInstances { exposed: vec![], used: vec![] };

    for instance in instances {
        match get_manifest(&instance.moniker, realm_query).await {
            Ok(decl) => {
                let (exposed, used) = capability_is_exposed_or_used(decl, &capability);
                if exposed {
                    matching_instances.exposed.push(instance.moniker.clone());
                }
                if used {
                    matching_instances.used.push(instance.moniker.clone());
                }
            }
            Err(GetManifestError::InstanceNotResolved(_)) => continue,
            Err(err) => {
                return Err(FindInstancesError::GetManifestError {
                    moniker: instance.moniker.clone(),
                    err,
                })
            }
        }
    }

    Ok(matching_instances)
}

/// Determine if |capability| is exposed or used by this component.
fn capability_is_exposed_or_used(manifest: ComponentDecl, capability: &str) -> (bool, bool) {
    let exposes = manifest.exposes;
    let uses = manifest.uses;

    let exposed = exposes.into_iter().any(|decl| {
        let name = match decl {
            ExposeDecl::Protocol(p) => p.target_name,
            ExposeDecl::Directory(d) => d.target_name,
            ExposeDecl::Service(s) => s.target_name,
            _ => {
                return false;
            }
        };
        name.to_string() == capability
    });

    let used = uses.into_iter().any(|decl| {
        let name = match decl {
            UseDecl::Protocol(p) => p.source_name,
            UseDecl::Directory(d) => d.source_name,
            UseDecl::Storage(s) => s.source_name,
            UseDecl::Service(s) => s.source_name,
            _ => {
                return false;
            }
        };
        name.to_string() == capability
    });

    (exposed, used)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::*;
    use fidl_fuchsia_component_decl as fdecl;
    use moniker::AbsoluteMonikerBase;
    use std::collections::HashMap;

    fn create_query() -> fsys::RealmQueryProxy {
        serve_realm_query(
            vec![fsys::Instance {
                moniker: Some("./my_foo".to_string()),
                url: Some("fuchsia-pkg://fuchsia.com/foo#meta/foo.cm".to_string()),
                instance_id: None,
                resolved_info: Some(fsys::ResolvedInfo {
                    resolved_url: Some("fuchsia-pkg://fuchsia.com/foo#meta/foo.cm".to_string()),
                    execution_info: None,
                    ..fsys::ResolvedInfo::EMPTY
                }),
                ..fsys::Instance::EMPTY
            }],
            HashMap::from([(
                "./my_foo".to_string(),
                fdecl::Component {
                    uses: Some(vec![fdecl::Use::Protocol(fdecl::UseProtocol {
                        source: Some(fdecl::Ref::Parent(fdecl::ParentRef)),
                        source_name: Some("fuchsia.foo.bar".to_string()),
                        target_path: Some("/svc/fuchsia.foo.bar".to_string()),
                        dependency_type: Some(fdecl::DependencyType::Strong),
                        availability: Some(fdecl::Availability::Required),
                        ..fdecl::UseProtocol::EMPTY
                    })]),
                    exposes: Some(vec![fdecl::Expose::Protocol(fdecl::ExposeProtocol {
                        source: Some(fdecl::Ref::Self_(fdecl::SelfRef)),
                        source_name: Some("fuchsia.bar.baz".to_string()),
                        target: Some(fdecl::Ref::Parent(fdecl::ParentRef)),
                        target_name: Some("fuchsia.bar.baz".to_string()),
                        ..fdecl::ExposeProtocol::EMPTY
                    })]),
                    ..fdecl::Component::EMPTY
                },
            )]),
            HashMap::new(),
        )
    }

    #[fuchsia::test]
    async fn uses_cml() {
        let query = create_query();

        let instances =
            find_instances_that_expose_or_use_capability("fuchsia.foo.bar".to_string(), &query)
                .await
                .unwrap();
        let exposed = instances.exposed;
        let mut used = instances.used;
        assert_eq!(used.len(), 1);
        assert!(exposed.is_empty());

        let moniker = used.remove(0);
        assert_eq!(moniker, AbsoluteMoniker::parse_str("/my_foo").unwrap());
    }

    #[fuchsia::test]
    async fn exposes_cml() {
        let query = create_query();

        let instances =
            find_instances_that_expose_or_use_capability("fuchsia.bar.baz".to_string(), &query)
                .await
                .unwrap();
        let mut exposed = instances.exposed;
        let used = instances.used;
        assert_eq!(exposed.len(), 1);
        assert!(used.is_empty());

        let moniker = exposed.remove(0);
        assert_eq!(moniker, AbsoluteMoniker::parse_str("/my_foo").unwrap());
    }
}
