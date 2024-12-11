// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::collections::HashMap;

use fuchsia_trace::duration;
use futures::AsyncWriteExt;
use traces::CATEGORY_MEMORY_CAPTURE;
use {fidl_fuchsia_kernel as fkernel, fidl_fuchsia_memory_attribution_plugin as fplugin};

use crate::attribution_client::{
    AttributionState, LocalPrincipalIdentifier, PrincipalDescription, PrincipalType,
};
use crate::common::GlobalPrincipalIdentifier;
use crate::resources::KernelResources;

/// Map between local and global PrincipalIdentifiers.
#[derive(Default)]
struct PrincipalIdMap(HashMap<LocalPrincipalIdentifier, GlobalPrincipalIdentifier>);

impl PrincipalIdMap {
    fn insert(&mut self, local_id: LocalPrincipalIdentifier, global_id: GlobalPrincipalIdentifier) {
        self.0.insert(local_id, global_id);
    }

    /// Returns the GlobalPrincipalIdentifier corresponding to `local_id`, provided by the
    /// Principal `parent_id`.
    fn get(
        &self,
        local_id: LocalPrincipalIdentifier,
        parent_id: GlobalPrincipalIdentifier,
    ) -> GlobalPrincipalIdentifier {
        if local_id == LocalPrincipalIdentifier::self_identifier() {
            parent_id
        } else {
            *self.0.get(&local_id).unwrap()
        }
    }
}

/// AttributionSnapshot holds and serves a snapshot of the memory of a Fuchsia system, to be sent
/// to a ffx command on a host.
pub struct AttributionSnapshot(fplugin::Snapshot);

impl AttributionSnapshot {
    pub fn new(
        attribution_state: AttributionState,
        kernel_resources: KernelResources,
        memory_stats: fkernel::MemoryStats,
        compression_stats: fkernel::MemoryStatsCompression,
    ) -> AttributionSnapshot {
        duration!(CATEGORY_MEMORY_CAPTURE, c"AttributionSnapshot::new");
        // Compute the capacity needed for |principals| and |attributions| to avoid reallocations
        // as we fill these vectors.
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
                principals.push(fplugin::Principal {
                    identifier: Some(definition.id.into()),
                    description: Some(match definition.description {
                        PrincipalDescription::Component(moniker) => {
                            fplugin::Description::Component(moniker)
                        }
                        PrincipalDescription::Part(part_name) => {
                            fplugin::Description::Part(part_name)
                        }
                    }),
                    principal_type: Some(match definition.principal_type {
                        PrincipalType::Runnable => fplugin::PrincipalType::Runnable,
                        PrincipalType::Part => fplugin::PrincipalType::Part,
                    }),
                    parent: definition
                        .attributor
                        .as_ref()
                        .map(|&principal_identifier| principal_identifier.into()),
                    ..Default::default()
                });
            }
            for (subject_identifier, resources) in attribution_provider.resources {
                attributions.push(fplugin::Attribution {
                    source: Some(provider_identifier.into()),
                    subject: Some(
                        local_to_global.get(subject_identifier, provider_identifier).into(),
                    ),
                    resources: Some(
                        resources
                            .into_iter()
                            .map(|r| match r {
                                fidl_fuchsia_memory_attribution::Resource::KernelObject(koid) => {
                                    fplugin::ResourceReference::KernelObject(koid)
                                }
                                fidl_fuchsia_memory_attribution::Resource::ProcessMapped(
                                    fidl_fuchsia_memory_attribution::ProcessMapped {
                                        process,
                                        base,
                                        len,
                                    },
                                ) => fplugin::ResourceReference::ProcessMapped(
                                    fplugin::ProcessMapped { process, base, len },
                                ),
                                fidl_fuchsia_memory_attribution::Resource::__SourceBreaking {
                                    unknown_ordinal: _,
                                } => unimplemented!("Unknown Resource type"),
                            })
                            .collect(),
                    ),
                    ..Default::default()
                });
            }
        }

        let mut resource_names =
            kernel_resources.resource_names.into_iter().collect::<Vec<(String, u64)>>();
        resource_names.sort_unstable_by_key(|(_, index)| *index);
        AttributionSnapshot(fplugin::Snapshot {
            attributions: Some(attributions),
            principals: Some(principals),
            resources: Some(kernel_resources.resources.into_values().collect()),
            resource_names: Some(resource_names.into_iter().map(|(name, _)| name).collect()),
            kernel_statistics: Some(fplugin::KernelStatistics {
                memory_stats: Some(memory_stats),
                compression_stats: Some(compression_stats),
                ..Default::default()
            }),
            ..Default::default()
        })
    }

    pub async fn serve(self, socket: zx::Socket) {
        duration!(CATEGORY_MEMORY_CAPTURE, c"AttributionSnapshot::serve");
        let mut asocket = fidl::AsyncSocket::from_socket(socket);

        let data = {
            duration!(CATEGORY_MEMORY_CAPTURE, c"AttributionSnapshot::serve persist");
            fidl::persist(&self.0).unwrap()
        };
        asocket.write_all(&data).await.unwrap();
    }
}
