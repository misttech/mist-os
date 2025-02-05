// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::attribution_client::AttributionClient;
use crate::common::PrincipalIdMap;
use crate::resources::{Job, KernelResources};
use attribution_processing::{
    Attribution, AttributionData, AttributionDataProvider, Principal, PrincipalDescription,
    PrincipalType, ResourceReference,
};
use fuchsia_sync::Mutex;
use fuchsia_trace::duration;
use futures::future::BoxFuture;
use futures::FutureExt;
use std::sync::Arc;
use traces::CATEGORY_MEMORY_CAPTURE;

pub struct AttributionDataProviderImpl {
    root_job: Mutex<Box<dyn Job>>,
    attribution_client: Arc<dyn AttributionClient>,
}

impl AttributionDataProviderImpl {
    /// Create a new [AttributionDataProviderImpl]. `attribution_client` exposes attribution
    /// information from the memory attribution protocol, and `root_job` is used to retrieve memory
    /// usage of kernel objects.
    pub fn new(
        attribution_client: Arc<dyn AttributionClient>,
        root_job: Mutex<Box<dyn Job>>,
    ) -> Arc<AttributionDataProviderImpl> {
        Arc::new(AttributionDataProviderImpl { root_job, attribution_client })
    }
}

impl AttributionDataProvider for AttributionDataProviderImpl {
    fn get_attribution_data(&self) -> BoxFuture<'_, Result<AttributionData, anyhow::Error>> {
        async {
            let attribution_state = self.attribution_client.get_attributions();
            let kernel_resources =
                KernelResources::get_resources(self.root_job.lock().as_ref(), &attribution_state)?;

            duration!(CATEGORY_MEMORY_CAPTURE, c"AttributionSnapshot::new");
            // Compute the capacity needed for |principals| and |attributions| to avoid
            // reallocations as we fill these vectors.
            let (num_principals, num_attributions) = attribution_state
                .0
                .values()
                .map(|provider| (provider.definitions.len(), provider.resources.len()))
                .fold((0, 0), |(acc_p, acc_a), (p, a)| (acc_p + p, acc_a + a));

            let mut principals = Vec::with_capacity(num_principals);
            let mut attributions = Vec::with_capacity(num_attributions);

            for (provider_identifier, attribution_provider) in attribution_state.0 {
                let mut local_to_global = PrincipalIdMap::default();
                for (local_id, definition) in attribution_provider.definitions {
                    local_to_global.insert(local_id, definition.id);
                    principals.push(Principal {
                        identifier: definition.id.into(),
                        description: match definition.description {
                            PrincipalDescription::Component(moniker) => {
                                attribution_processing::PrincipalDescription::Component(moniker)
                            }
                            PrincipalDescription::Part(part_name) => {
                                attribution_processing::PrincipalDescription::Part(part_name)
                            }
                        },
                        principal_type: match definition.principal_type {
                            PrincipalType::Runnable => {
                                attribution_processing::PrincipalType::Runnable
                            }
                            PrincipalType::Part => attribution_processing::PrincipalType::Part,
                        },
                        parent: definition
                            .attributor
                            .as_ref()
                            .map(|&principal_identifier| principal_identifier.into()),
                    });
                }
                for (subject_identifier, resources) in attribution_provider.resources {
                    attributions.push(Attribution {
                        source: provider_identifier.into(),
                        subject: local_to_global
                            .get(subject_identifier, provider_identifier)
                            .into(),
                        resources: resources
                            .into_iter()
                            .map(|r| match r {
                                fidl_fuchsia_memory_attribution::Resource::KernelObject(koid) => {
                                    ResourceReference::KernelObject(koid)
                                }
                                fidl_fuchsia_memory_attribution::Resource::ProcessMapped(
                                    fidl_fuchsia_memory_attribution::ProcessMapped {
                                        process,
                                        base,
                                        len,
                                    },
                                ) => ResourceReference::ProcessMapped { process, base, len },
                                fidl_fuchsia_memory_attribution::Resource::__SourceBreaking {
                                    unknown_ordinal: _,
                                } => unimplemented!("Unknown Resource type"),
                            })
                            .collect(),
                    });
                }
            }

            let mut resource_names =
                kernel_resources.resource_names.into_iter().collect::<Vec<(String, u64)>>();
            resource_names.sort_unstable_by_key(|(_, index)| *index);

            Ok(AttributionData {
                principals_vec: principals,
                resources_vec: kernel_resources.resources.into_values().map(|r| r.into()).collect(),
                resource_names: resource_names.into_iter().map(|(name, _)| name).collect(),
                attributions,
            })
        }
        .boxed()
    }
}

#[cfg(test)]
mod tests {
    use core::assert_eq;
    use std::collections::HashSet;

    use attribution_processing::PrincipalIdentifier;
    use fidl_fuchsia_memory_attribution as fattribution;

    use super::*;
    use crate::attribution_client::{AttributionProvider, PrincipalDefinition};
    use crate::common::{
        GlobalPrincipalIdentifier, GlobalPrincipalIdentifierFactory, LocalPrincipalIdentifier,
    };
    use crate::resources::tests::{simple_vmo_info, FakeJob, FakeProcess};
    use fuchsia_async as fasync;

    #[test]
    fn test_get_capture() {
        let mut exec = fasync::TestExecutor::new();
        let mut identifier_factory = GlobalPrincipalIdentifierFactory::default();

        #[derive(Default)]
        struct FakeAttributionClient {
            state: crate::attribution_client::AttributionState,
        }

        impl FakeAttributionClient {
            fn add_provider_vmo(
                &mut self,
                parent_id: GlobalPrincipalIdentifier,
                principal_id: GlobalPrincipalIdentifier,
                name: &str,
                resource_koid: u64,
            ) {
                self.state.0.insert(
                    parent_id.clone(),
                    AttributionProvider {
                        definitions: [(
                            LocalPrincipalIdentifier(1),
                            PrincipalDefinition {
                                attributor: Some(parent_id),
                                id: principal_id,
                                description: PrincipalDescription::Component(name.to_owned()),
                                principal_type: PrincipalType::Runnable,
                            },
                        )]
                        .into(),
                        resources: [(
                            LocalPrincipalIdentifier(1),
                            [fattribution::Resource::KernelObject(resource_koid)].into(),
                        )]
                        .into(),
                    },
                );
            }

            fn add_provider_part_map(
                &mut self,
                parent_id: GlobalPrincipalIdentifier,
                principal_id: GlobalPrincipalIdentifier,
                name: &str,
                process_koid: u64,
                base: u64,
                len: u64,
            ) {
                self.state.0.insert(
                    parent_id.clone(),
                    AttributionProvider {
                        definitions: [(
                            LocalPrincipalIdentifier(1),
                            PrincipalDefinition {
                                attributor: Some(parent_id),
                                id: principal_id,
                                description: PrincipalDescription::Part(name.to_owned()),
                                principal_type: PrincipalType::Part,
                            },
                        )]
                        .into(),
                        resources: [(
                            LocalPrincipalIdentifier(1),
                            [fattribution::Resource::ProcessMapped(fattribution::ProcessMapped {
                                process: process_koid,
                                base,
                                len,
                            })]
                            .into(),
                        )]
                        .into(),
                    },
                );
            }
        }

        impl AttributionClient for FakeAttributionClient {
            fn get_attributions(&self) -> crate::attribution_client::AttributionState {
                self.state.clone()
            }
        }

        let mut fake_attribution_client = FakeAttributionClient::default();
        let parent_global_id = identifier_factory.next();
        let component_global_id = identifier_factory.next();

        fake_attribution_client.add_provider_vmo(
            parent_global_id,
            component_global_id.clone(),
            "component1",
            2,
        );
        fake_attribution_client.add_provider_part_map(
            component_global_id,
            identifier_factory.next(),
            "part2",
            5,
            0,
            1024,
        );

        let mut mapping_details = zx::MappingDetails::default();
        mapping_details.vmo_koid = zx::Koid::from_raw(6);
        mapping_details.committed_bytes = 1024;

        let capture_provider = AttributionDataProviderImpl::new(
            Arc::new(fake_attribution_client),
            Mutex::new(Box::new(FakeJob::new(
                1,
                "job1",
                [FakeJob::new(
                    2,
                    "job2",
                    [].into(),
                    [
                        FakeProcess::new(
                            3,
                            "process3",
                            [
                                simple_vmo_info(4, "vmo4", 0, 1024, 2048),
                                simple_vmo_info(6, "vmo6", 0, 1024, 2048),
                            ]
                            .into(),
                            [].into(),
                        ),
                        FakeProcess::new(
                            5,
                            "process5",
                            [].into(),
                            [zx::MapInfo::new(
                                zx::Name::from_bytes_lossy("map1".as_bytes()),
                                0,
                                1024,
                                1,
                                zx::MapDetails::Mapping(&mapping_details),
                            )
                            .unwrap()]
                            .into(),
                        ),
                    ]
                    .into(),
                )]
                .into(),
                [].into(),
            ))),
        );
        let capture = exec
            .run_singlethreaded(async { capture_provider.get_attribution_data().await })
            .unwrap();
        assert_eq!(
            HashSet::from_iter(capture.principals_vec.into_iter()),
            HashSet::from([
                Principal {
                    identifier: PrincipalIdentifier(2),
                    parent: Some(PrincipalIdentifier(1)),
                    description: PrincipalDescription::Component("component1".to_owned()),
                    principal_type: PrincipalType::Runnable,
                },
                Principal {
                    identifier: PrincipalIdentifier(3),
                    parent: Some(PrincipalIdentifier(2)),
                    description: PrincipalDescription::Part("part2".to_owned()),
                    principal_type: PrincipalType::Part,
                }
            ])
        );

        assert_eq!(
            capture.resource_names.into_iter().collect::<HashSet<String>>(),
            HashSet::from([
                "job1".to_owned(),
                "job2".to_owned(),
                "process3".to_owned(),
                "vmo4".to_owned(),
                "process5".to_owned(),
                "vmo6".to_owned()
            ])
        );
    }
}
