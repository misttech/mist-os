// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod json;
mod output;

#[macro_use]
extern crate prettytable;
use crate::output::{KernelStatistics, PluginOutput};
use anyhow::Result;
use async_trait::async_trait;
use errors::ffx_error;
use ffx_profile_memory_components_args::ComponentsCommand;
use fho::{moniker, AvailabilityFlag, FfxMain, FfxTool, SimpleWriter};
use futures::AsyncReadExt;
use json::JsonConvertible;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};

use fidl_fuchsia_memory_attribution_plugin as fplugin;

#[derive(FfxTool)]
#[check(AvailabilityFlag("ffx_profile_memory_components"))]
pub struct MemoryComponentsTool {
    #[command]
    cmd: ComponentsCommand,
    #[with(moniker("/core/memory_monitor2"))]
    monitor_proxy: fplugin::MemoryMonitorProxy,
}

fho::embedded_plugin!(MemoryComponentsTool);

#[async_trait(?Send)]
impl FfxMain for MemoryComponentsTool {
    type Writer = SimpleWriter;

    /// Forwards the specified memory pressure level to the fuchsia.memory.debug.MemoryPressure FIDL
    /// interface.
    async fn main(self, _writer: Self::Writer) -> fho::Result<()> {
        let snapshot = match self.cmd.stdin_input {
            false => self.load_from_device().await?,
            true => {
                fplugin::Snapshot::from_json(&serde_json::from_reader(std::io::stdin()).unwrap())
                    .unwrap()
            }
        };
        if self.cmd.debug_json {
            println!("{}", serde_json::to_string(&snapshot.to_json()).unwrap());
        } else {
            let output = process_snapshot(snapshot);
            println!("{}", output);
        }
        Ok(())
    }
}

impl MemoryComponentsTool {
    async fn load_from_device(&self) -> fho::Result<fplugin::Snapshot> {
        let (client_end, server_end) = fidl::Socket::create_stream();
        let mut client_socket = fidl::AsyncSocket::from_socket(client_end);

        self.monitor_proxy
            .get_snapshot(server_end)
            .map_err(|err| ffx_error!("Failed to call MemoryMonitorProxy/GetSnapshot: {err}"))?;

        let mut buffer: Vec<u8> = Vec::new();
        client_socket
            .read_to_end(&mut buffer)
            .await
            .map_err(|err| ffx_error!("Failed to read socket: {err}"))?;
        let snapshot: fplugin::Snapshot = fidl::unpersist(&buffer)
            .map_err(|err| ffx_error!("Failed to unpersist elements: {err}"))?;
        Ok(snapshot)
    }
}

#[derive(PartialEq, Eq, Hash, Clone, Copy, Debug)]
struct PrincipalIdentifier(u64);

impl From<fplugin::PrincipalIdentifier> for PrincipalIdentifier {
    fn from(value: fplugin::PrincipalIdentifier) -> Self {
        PrincipalIdentifier(value.id)
    }
}

/// A Principal, that can use and claim memory.
struct Principal {
    // These fields are initialized from [fplugin::Principal].
    identifier: PrincipalIdentifier,
    description: fplugin::Description,
    principal_type: fplugin::PrincipalType,

    /// Principal that declared this Principal. None if this Principal is at the root of the system
    /// hierarchy (the root principal is a statically defined Principal encompassing all resources
    /// on the system). The Principal hierarchy forms a tree (no cycles).
    parent: Option<PrincipalIdentifier>,

    // These fields are computed from the rest of the [fplugin::Snapshot] data.
    /// Map of attribution claims made about this Principal (this Principal is the subject of the
    /// claim). This map goes from the source Principal to the attribution claim.
    attribution_claims: HashMap<PrincipalIdentifier, fplugin::Attribution>,
    /// KOIDs of resources attributed to this principal, after resolution of sharing and
    /// reattributions.
    resources: HashSet<u64>,
}

/// Creates a new [Principal] from a [fplugin::Principal] object. The [Principal] object will
/// contain all the data from [fplugin::Principal] and have its other fields initialized empty.
impl From<fplugin::Principal> for Principal {
    fn from(value: fplugin::Principal) -> Self {
        Principal {
            identifier: value.identifier.unwrap().into(),
            description: value.description.unwrap(),
            principal_type: value.principal_type.unwrap(),
            parent: value.parent.map(Into::into),
            attribution_claims: HashMap::new(),
            resources: HashSet::new(),
        }
    }
}

impl Principal {
    fn name(&self) -> &str {
        match &self.description {
            fplugin::Description::Component(component_name) => component_name,
            fplugin::Description::Part(part_name) => part_name,
            fplugin::DescriptionUnknown!() => unimplemented!(),
        }
    }
}

/// Type of the claim, that changes depending on how the claim was created.
#[derive(Clone, Copy, Eq, PartialEq, Hash, Debug)]
enum ClaimType {
    /// A principal claimed this resource directly.
    Direct,
    /// A principal claimed a resource that contains this resource (e.g. a process containing a
    /// VMO).
    Indirect,
    /// A principal claimed a child of this resource (e.g. a copy-on-write VMO child of this VMO).
    Child,
}

/// Attribution claim of a Principal on a Resource.
///
/// Note that this object is slightly different from the [fplugin::Attribution] object: it goes from
/// a Resource to a Principal, and covers also indirect attribution claims of sub- and parent
/// resources.
#[derive(Clone, Copy, Eq, PartialEq, Hash, Debug)]
struct Claim {
    /// Principal to which the resources are attributed.
    subject: PrincipalIdentifier,
    /// Principal making the attribution claim.
    source: PrincipalIdentifier,
    claim_type: ClaimType,
}

#[derive(Debug)]
pub struct Resource {
    koid: u64,
    name: String,
    resource_type: fplugin::ResourceType,
    claims: HashSet<Claim>,
}

// Claim with a boolean tag, to help find leaves in the claim assignment graph.
struct TaggedClaim(Claim, bool);

impl Resource {
    fn from(value: fplugin::Resource, resource_names: &Vec<String>) -> Self {
        Resource {
            koid: value.koid.unwrap(),
            name: resource_names.get(value.name_index.unwrap() as usize).unwrap().to_owned(),
            resource_type: value.resource_type.unwrap(),
            claims: HashSet::new(),
        }
    }

    fn children(&self) -> Vec<u64> {
        match &self.resource_type {
            fplugin::ResourceType::Job(job) => {
                let mut r: Vec<u64> = job.child_jobs.iter().flatten().map(|k| *k).collect();
                r.extend(job.processes.iter().flatten().map(|k| *k));
                if r.len() == 0 {
                    eprintln!("{} has no processes", self.name);
                }
                r
            }
            fplugin::ResourceType::Process(process) => {
                process.vmos.iter().flatten().map(|k| *k).collect()
            }
            fplugin::ResourceType::Vmo(_) => Vec::new(),
            _ => todo!(),
        }
    }

    /// Process the claims made on this resource to disambiguate between reassignment and sharing.
    ///
    /// [process_claims] looks at each claim made on this resource, and removes claims that are
    /// reassigned by another claim. This happens if a principal A gives a resource to principal B,
    /// and B then gives it to principal C. However, if two independent principals claim this
    /// resource, then both their claims are kept.
    /// This is done by:
    /// (i)  preserving all self claims, and
    /// (ii) preserving only leaves in the DAG following claim.source to claim.subject edges.
    fn process_claims(&mut self) {
        let mut claims_by_source: HashMap<PrincipalIdentifier, RefCell<Vec<TaggedClaim>>> =
            Default::default();
        let mut self_claims = Vec::new();

        for claim in self.claims.iter().cloned() {
            if claim.source == claim.subject {
                // Self claims are taken out of the graph because they are never transferred. This
                // is to implement sharing.
                self_claims.push(claim);
            } else {
                claims_by_source
                    .entry(claim.source)
                    .or_default()
                    .borrow_mut()
                    .push(TaggedClaim(claim, false));
            }
        }

        self.claims = self_claims.into_iter().collect();
        for (_, claimlist_refcell) in claims_by_source.iter() {
            let mut claimlist = claimlist_refcell.borrow_mut();
            for tagged_claim in claimlist.iter_mut() {
                self.claims.extend(
                    Resource::process_claims_recursive(tagged_claim, &claims_by_source).into_iter(),
                );
            }
        }
    }

    /// Recursively look at claims to find the ones that are not reassigned.
    fn process_claims_recursive(
        tagged_claim: &mut TaggedClaim,
        claims: &HashMap<PrincipalIdentifier, RefCell<Vec<TaggedClaim>>>,
    ) -> Vec<Claim> {
        let claim = match tagged_claim.1 {
            true => {
                // We have visited this claim already, we can skip.
                return vec![];
            }
            false => {
                // We tag visited claims, so we don't visit them again.
                tagged_claim.1 = true;
                tagged_claim.0
            }
        };
        let subject = &claim.subject;
        // We find if this claim has been reassigned.
        let mut subject_claims = match claims.get(subject) {
            Some(value_ref) => {
                // [subject_claims] mutable borrow is held when recursing below, and
                // [RefCell::try_borrow_mut] returns an error if called when a mutable borrow is
                // already held. This ensures an error will be thrown at runtime if there is a
                // cycle.
                value_ref.try_borrow_mut().expect("Claims form a cycle, this is not supported")
            }
            None => {
                // The claim is not reassigned, we keep the claim.
                return vec![claim];
            }
        };
        let mut leaves = vec![];
        for subject_claim in subject_claims.iter_mut() {
            leaves.append(&mut Resource::process_claims_recursive(subject_claim, claims));
        }
        leaves
    }
}

/// Process a [fplugin::Snapshot] to resolve claims.
fn process_snapshot(snapshot: fplugin::Snapshot) -> PluginOutput {
    // Map from moniker token ID to Principal struct.
    let principals: HashMap<PrincipalIdentifier, RefCell<Principal>> = snapshot
        .principals
        .into_iter()
        .flatten()
        .map(|p| (p.identifier.unwrap().into(), RefCell::new(p.into())))
        .collect();

    // Map from kernel resource koid to Resource struct.
    let mut resources: HashMap<u64, RefCell<Resource>> = snapshot
        .resources
        .into_iter()
        .flatten()
        .map(|r| {
            (
                r.koid.unwrap(),
                RefCell::new(Resource::from(r, snapshot.resource_names.as_ref().unwrap())),
            )
        })
        .collect();
    // Map from subject moniker token ID to Attribution struct.
    let attributions = snapshot.attributions.unwrap();

    // Add direct claims to resources.
    for attribution in attributions {
        principals.get(&attribution.subject.clone().unwrap().into()).map(|p| {
            p.borrow_mut()
                .attribution_claims
                .insert(attribution.source.unwrap().into(), attribution.clone())
        });
        for resource in attribution.resources.unwrap() {
            match resource {
                fplugin::ResourceReference::KernelObject(koid) => {
                    if !resources.contains_key(&koid) {
                        continue;
                    }
                    resources.get_mut(&koid).unwrap().get_mut().claims.insert(Claim {
                        source: attribution.source.unwrap().into(),
                        subject: attribution.subject.unwrap().into(),
                        claim_type: ClaimType::Direct,
                    });
                }
                fplugin::ResourceReference::ProcessMapped(fplugin::ProcessMapped {
                    process,
                    base,
                    len,
                }) => {
                    if !resources.contains_key(&process) {
                        continue;
                    }
                    let mut matched_vmos = Vec::new();
                    if let fplugin::ResourceType::Process(process_data) =
                        &resources.get(&process).unwrap().borrow().resource_type
                    {
                        for mapping in process_data.mappings.iter().flatten() {
                            // We consider an entire VMO to be matched if it has a mapping
                            // within the claimed region.
                            if mapping.address_base.unwrap() >= base
                                && mapping.address_base.unwrap() + mapping.size.unwrap()
                                    <= base + len
                            {
                                matched_vmos.push(mapping.vmo.unwrap());
                            }
                        }
                    }
                    for vmo_koid in matched_vmos {
                        match resources.get_mut(&vmo_koid) {
                            Some(resource) => {
                                resource.get_mut().claims.insert(Claim {
                                    source: attribution.source.unwrap().into(),
                                    subject: attribution.subject.unwrap().into(),
                                    claim_type: ClaimType::Direct,
                                });
                            }
                            None => {
                                // The VMO is unknown. This can happen when a VMO is created between
                                // the collection of the list of VMOs and the collection of the
                                // process mappings.
                            }
                        }
                    }
                }
                fplugin::ResourceReference::__SourceBreaking { unknown_ordinal: _ } => {}
            }
        }
    }

    // Propagate claims. We propagate direct claims to child resources recursively until we hit a
    // resource that is directly claimed: this is because we consider that attributors deeper in the
    // principal hierarchy will not attribute resources higher in the resource hierarchy than the
    // ones attributed by their ancestors (ie. attribution is always more precise as we go deeper).
    for (_, resource_refcell) in &resources {
        let resource = resource_refcell.borrow_mut();
        // Extract the list of direct claims to propagate.
        let direct_claims: Vec<&Claim> = resource
            .claims
            .iter()
            .filter(|claim| match claim.claim_type {
                ClaimType::Direct => true,
                _ => false,
            })
            .collect();

        if direct_claims.is_empty() {
            // There is no direct claim to propagate, we can skip this resource.
            continue;
        }

        let propagated_claims: Vec<Claim> = direct_claims
            .into_iter()
            .map(|claim| Claim {
                source: claim.source,
                subject: claim.subject,
                claim_type: ClaimType::Indirect,
            })
            .collect();
        let mut frontier = Vec::new();
        frontier.extend(resource.children());
        while !frontier.is_empty() {
            let child = frontier.pop().unwrap();
            let mut child_resource = match resources.get(&child) {
                Some(resource) => resource.borrow_mut(),
                None => {
                    // This can happen if a resource is created or disappears while we were
                    // collecting information about all the resources in the system. This should
                    // remain a rare event.
                    println!("Resource {} not found", child);
                    continue;
                }
            };
            if child_resource.claims.iter().any(|c| c.claim_type == ClaimType::Direct) {
                // If there is a direct claim on the resource, don't propagate.
                continue;
            }
            child_resource.claims.extend(propagated_claims.clone().iter());
            frontier.extend(child_resource.children().iter());
        }
    }

    for (_, resource_refcell) in &resources {
        let mut resource = resource_refcell.borrow_mut();
        resource.process_claims();
    }

    // Push claimed resources to principals. We are interested in VMOs as the VMOs are the resources
    // actually holding memory. We also keep track of the process to display its name in the output.
    for (resource_id, resource_refcell) in &resources {
        let resource = resource_refcell.borrow();
        if let fplugin::ResourceType::Vmo(vmo) = &resource.resource_type {
            let mut ancestors = vec![*resource_id];
            let mut current_parent = vmo.parent;
            // Add the parents of a VMO as "Child" claims. This is done so that clones of VMOs,
            // with possibly no memory of their own, get attributed the resources of their parent.
            while let Some(parent_koid) = current_parent {
                if parent_koid == 0 {
                    panic!("Parent is not None but 0.");
                }
                ancestors.push(parent_koid);
                let mut current_resource = match resources.get(&parent_koid) {
                    Some(res) => res.borrow_mut(),
                    None => break,
                };
                current_resource.claims.extend(resource.claims.iter().map(|c| Claim {
                    subject: c.subject,
                    source: c.source,
                    claim_type: ClaimType::Child,
                }));
                current_parent = match &current_resource.resource_type {
                    fplugin::ResourceType::Job(_) => panic!("This should not happen"),
                    fplugin::ResourceType::Process(_) => panic!("This should not happen"),
                    fplugin::ResourceType::Vmo(current_vmo) => current_vmo.parent,
                    _ => unimplemented!(),
                };
            }

            for claim in &resource.claims {
                principals
                    .get(&claim.subject)
                    .unwrap()
                    .borrow_mut()
                    .resources
                    .extend(ancestors.iter());
            }
        } else if let fplugin::ResourceType::Process(_) = &resource.resource_type {
            for claim in &resource.claims {
                principals
                    .get(&claim.subject)
                    .unwrap()
                    .borrow_mut()
                    .resources
                    .insert(resource.koid);
            }
        }
    }

    let raw_kernel_stats = snapshot.kernel_statistics.unwrap();
    let kernel_stats = KernelStatistics {
        total: raw_kernel_stats.memory_stats.as_ref().unwrap().total_bytes.unwrap(),
        free: raw_kernel_stats.memory_stats.as_ref().unwrap().free_bytes.unwrap(),
        kernel_total: raw_kernel_stats.memory_stats.as_ref().unwrap().wired_bytes.unwrap()
            + raw_kernel_stats.memory_stats.as_ref().unwrap().total_heap_bytes.unwrap()
            + raw_kernel_stats.memory_stats.as_ref().unwrap().mmu_overhead_bytes.unwrap()
            + raw_kernel_stats.memory_stats.as_ref().unwrap().ipc_bytes.unwrap(),
        wired: raw_kernel_stats.memory_stats.as_ref().unwrap().wired_bytes.unwrap(),
        total_heap: raw_kernel_stats.memory_stats.as_ref().unwrap().total_heap_bytes.unwrap(),
        vmo: raw_kernel_stats.memory_stats.as_ref().unwrap().vmo_bytes.unwrap(),
        mmu: raw_kernel_stats.memory_stats.as_ref().unwrap().mmu_overhead_bytes.unwrap(),
        ipc: raw_kernel_stats.memory_stats.as_ref().unwrap().ipc_bytes.unwrap(),
        other: raw_kernel_stats.memory_stats.as_ref().unwrap().other_bytes.unwrap(),
        zram_compressed_total: raw_kernel_stats.memory_stats.as_ref().unwrap().zram_bytes.unwrap(),
        vmo_reclaim_total_bytes: raw_kernel_stats
            .memory_stats
            .as_ref()
            .unwrap()
            .vmo_reclaim_total_bytes
            .unwrap(),
        vmo_reclaim_newest_bytes: raw_kernel_stats
            .memory_stats
            .as_ref()
            .unwrap()
            .vmo_reclaim_newest_bytes
            .unwrap(),
        vmo_reclaim_oldest_bytes: raw_kernel_stats
            .memory_stats
            .as_ref()
            .unwrap()
            .vmo_reclaim_oldest_bytes
            .unwrap(),
        vmo_reclaim_disabled_bytes: raw_kernel_stats
            .memory_stats
            .as_ref()
            .unwrap()
            .vmo_reclaim_disabled_bytes
            .unwrap(),
        vmo_discardable_locked_bytes: raw_kernel_stats
            .memory_stats
            .as_ref()
            .unwrap()
            .vmo_discardable_locked_bytes
            .unwrap(),
        vmo_discardable_unlocked_bytes: raw_kernel_stats
            .memory_stats
            .as_ref()
            .unwrap()
            .vmo_discardable_unlocked_bytes
            .unwrap(),
    };
    PluginOutput::build(principals, resources, kernel_stats)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fidl_fuchsia_memory_attribution_plugin as fplugin;

    #[test]
    fn test_gather_resources() {
        // Create a fake snapshot with 4 principals:
        // root (0)
        //  - runner (1)
        //    - component 3 (3)
        //  - component 2 (2)
        //
        // and the following job/process/vmo hierarchy:
        // root_job (1000)
        //  * root_process (1001)
        //    . root_vmo (1002)
        //    . shared_vmo (1003)
        //  - runner_job (1004)
        //    * runner_process (1005)
        //      . runner_vmo (1006)
        //      . component_vmo (1007)
        //      . component_vmo2 (1012)
        //      . component_vmo3 (1013)
        //  - component_2_job (1008)
        //    * 2_process (1009)
        //      . 2_vmo (1010)
        //      . shared_vmo (1003)
        // And an additional parent VMO for 2_vmo, 2_vmo_parent (1011).

        let snapshot = fplugin::Snapshot {
            attributions: Some(vec![
                fplugin::Attribution {
                    source: Some(fplugin::PrincipalIdentifier { id: 0 }),
                    subject: Some(fplugin::PrincipalIdentifier { id: 0 }),
                    resources: Some(vec![fplugin::ResourceReference::KernelObject(1000)]),
                    ..Default::default()
                },
                fplugin::Attribution {
                    source: Some(fplugin::PrincipalIdentifier { id: 0 }),
                    subject: Some(fplugin::PrincipalIdentifier { id: 1 }),
                    resources: Some(vec![fplugin::ResourceReference::KernelObject(1004)]),
                    ..Default::default()
                },
                fplugin::Attribution {
                    source: Some(fplugin::PrincipalIdentifier { id: 0 }),
                    subject: Some(fplugin::PrincipalIdentifier { id: 2 }),
                    resources: Some(vec![fplugin::ResourceReference::KernelObject(1008)]),
                    ..Default::default()
                },
                fplugin::Attribution {
                    source: Some(fplugin::PrincipalIdentifier { id: 1 }),
                    subject: Some(fplugin::PrincipalIdentifier { id: 3 }),
                    resources: Some(vec![
                        fplugin::ResourceReference::KernelObject(1007),
                        fplugin::ResourceReference::ProcessMapped(fplugin::ProcessMapped {
                            process: 1005,
                            base: 1024,
                            len: 1024,
                        }),
                    ]),
                    ..Default::default()
                },
            ]),
            principals: Some(vec![
                fplugin::Principal {
                    identifier: Some(fplugin::PrincipalIdentifier { id: 0 }),
                    description: Some(fplugin::Description::Component("root".to_owned())),
                    principal_type: Some(fplugin::PrincipalType::Runnable),
                    parent: None,
                    ..Default::default()
                },
                fplugin::Principal {
                    identifier: Some(fplugin::PrincipalIdentifier { id: 1 }),
                    description: Some(fplugin::Description::Component("runner".to_owned())),
                    principal_type: Some(fplugin::PrincipalType::Runnable),
                    parent: Some(fplugin::PrincipalIdentifier { id: 0 }),
                    ..Default::default()
                },
                fplugin::Principal {
                    identifier: Some(fplugin::PrincipalIdentifier { id: 2 }),
                    description: Some(fplugin::Description::Component("component 2".to_owned())),
                    principal_type: Some(fplugin::PrincipalType::Runnable),
                    parent: Some(fplugin::PrincipalIdentifier { id: 0 }),
                    ..Default::default()
                },
                fplugin::Principal {
                    identifier: Some(fplugin::PrincipalIdentifier { id: 3 }),
                    description: Some(fplugin::Description::Component("component 3".to_owned())),
                    principal_type: Some(fplugin::PrincipalType::Runnable),
                    parent: Some(fplugin::PrincipalIdentifier { id: 1 }),
                    ..Default::default()
                },
            ]),
            resources: Some(vec![
                fplugin::Resource {
                    koid: Some(1000),
                    name_index: Some(0),
                    resource_type: Some(fplugin::ResourceType::Job(fplugin::Job {
                        child_jobs: Some(vec![1004, 1008]),
                        processes: Some(vec![1001]),
                        ..Default::default()
                    })),
                    ..Default::default()
                },
                fplugin::Resource {
                    koid: Some(1001),
                    name_index: Some(1),
                    resource_type: Some(fplugin::ResourceType::Process(fplugin::Process {
                        vmos: Some(vec![1002, 1003]),
                        mappings: None,
                        ..Default::default()
                    })),
                    ..Default::default()
                },
                fplugin::Resource {
                    koid: Some(1002),
                    name_index: Some(2),
                    resource_type: Some(fplugin::ResourceType::Vmo(fplugin::Vmo {
                        committed_bytes: Some(1024),
                        populated_bytes: Some(2048),
                        ..Default::default()
                    })),
                    ..Default::default()
                },
                fplugin::Resource {
                    koid: Some(1003),
                    name_index: Some(3),
                    resource_type: Some(fplugin::ResourceType::Vmo(fplugin::Vmo {
                        committed_bytes: Some(1024),
                        populated_bytes: Some(2048),
                        ..Default::default()
                    })),
                    ..Default::default()
                },
                fplugin::Resource {
                    koid: Some(1004),
                    name_index: Some(4),
                    resource_type: Some(fplugin::ResourceType::Job(fplugin::Job {
                        child_jobs: Some(vec![]),
                        processes: Some(vec![1005]),
                        ..Default::default()
                    })),
                    ..Default::default()
                },
                fplugin::Resource {
                    koid: Some(1005),
                    name_index: Some(5),
                    resource_type: Some(fplugin::ResourceType::Process(fplugin::Process {
                        vmos: Some(vec![1006, 1007, 1012]),
                        mappings: Some(vec![
                            fplugin::Mapping {
                                vmo: Some(1006),
                                address_base: Some(0),
                                size: Some(512),
                                ..Default::default()
                            },
                            fplugin::Mapping {
                                vmo: Some(1012),
                                address_base: Some(1024),
                                size: Some(512),
                                ..Default::default()
                            },
                            fplugin::Mapping {
                                vmo: Some(1013),
                                address_base: Some(1536),
                                size: Some(512),
                                ..Default::default()
                            },
                            fplugin::Mapping {
                                vmo: Some(1006),
                                address_base: Some(2048),
                                size: Some(512),
                                ..Default::default()
                            },
                        ]),
                        ..Default::default()
                    })),
                    ..Default::default()
                },
                fplugin::Resource {
                    koid: Some(1006),
                    name_index: Some(6),
                    resource_type: Some(fplugin::ResourceType::Vmo(fplugin::Vmo {
                        committed_bytes: Some(1024),
                        populated_bytes: Some(2048),
                        ..Default::default()
                    })),
                    ..Default::default()
                },
                fplugin::Resource {
                    koid: Some(1007),
                    name_index: Some(7),
                    resource_type: Some(fplugin::ResourceType::Vmo(fplugin::Vmo {
                        committed_bytes: Some(128),
                        populated_bytes: Some(256),
                        ..Default::default()
                    })),
                    ..Default::default()
                },
                fplugin::Resource {
                    koid: Some(1008),
                    name_index: Some(8),
                    resource_type: Some(fplugin::ResourceType::Job(fplugin::Job {
                        child_jobs: Some(vec![]),
                        processes: Some(vec![1009]),
                        ..Default::default()
                    })),
                    ..Default::default()
                },
                fplugin::Resource {
                    koid: Some(1009),
                    name_index: Some(9),
                    resource_type: Some(fplugin::ResourceType::Process(fplugin::Process {
                        vmos: Some(vec![1010, 1003]),
                        mappings: None,
                        ..Default::default()
                    })),
                    ..Default::default()
                },
                fplugin::Resource {
                    koid: Some(1010),
                    name_index: Some(10),
                    resource_type: Some(fplugin::ResourceType::Vmo(fplugin::Vmo {
                        committed_bytes: Some(1024),
                        populated_bytes: Some(2048),
                        parent: Some(1011),
                        ..Default::default()
                    })),
                    ..Default::default()
                },
                fplugin::Resource {
                    koid: Some(1011),
                    name_index: Some(11),
                    resource_type: Some(fplugin::ResourceType::Vmo(fplugin::Vmo {
                        committed_bytes: Some(1024),
                        populated_bytes: Some(2048),
                        ..Default::default()
                    })),
                    ..Default::default()
                },
                fplugin::Resource {
                    koid: Some(1012),
                    name_index: Some(12),
                    resource_type: Some(fplugin::ResourceType::Vmo(fplugin::Vmo {
                        committed_bytes: Some(1024),
                        populated_bytes: Some(2048),
                        ..Default::default()
                    })),
                    ..Default::default()
                },
                fplugin::Resource {
                    koid: Some(1013),
                    name_index: Some(13),
                    resource_type: Some(fplugin::ResourceType::Vmo(fplugin::Vmo {
                        committed_bytes: Some(1024),
                        populated_bytes: Some(2048),
                        ..Default::default()
                    })),
                    ..Default::default()
                },
            ]),
            resource_names: Some(vec![
                "root_job".to_owned(),
                "root_process".to_owned(),
                "root_vmo".to_owned(),
                "shared_vmo".to_owned(),
                "runner_job".to_owned(),
                "runner_process".to_owned(),
                "runner_vmo".to_owned(),
                "component_vmo".to_owned(),
                "component_2_job".to_owned(),
                "2_process".to_owned(),
                "2_vmo".to_owned(),
                "2_vmo_parent".to_owned(),
                "component_vmo_mapped".to_owned(),
                "component_vmo_mapped2".to_owned(),
            ]),
            kernel_statistics: Some(fplugin::KernelStatistics {
                memory_stats: Some(fidl_fuchsia_kernel::MemoryStats {
                    total_bytes: Some(1),
                    free_bytes: Some(2),
                    free_loaned_bytes: Some(3),
                    wired_bytes: Some(4),
                    total_heap_bytes: Some(5),
                    free_heap_bytes: Some(6),
                    vmo_bytes: Some(7),
                    mmu_overhead_bytes: Some(8),
                    ipc_bytes: Some(9),
                    cache_bytes: Some(10),
                    slab_bytes: Some(11),
                    zram_bytes: Some(12),
                    other_bytes: Some(13),
                    vmo_reclaim_total_bytes: Some(14),
                    vmo_reclaim_newest_bytes: Some(15),
                    vmo_reclaim_oldest_bytes: Some(16),
                    vmo_reclaim_disabled_bytes: Some(17),
                    vmo_discardable_locked_bytes: Some(18),
                    vmo_discardable_unlocked_bytes: Some(19),
                    ..Default::default()
                }),
                compression_stats: Some(fidl_fuchsia_kernel::MemoryStatsCompression {
                    uncompressed_storage_bytes: Some(15),
                    compressed_storage_bytes: Some(16),
                    compressed_fragmentation_bytes: Some(17),
                    compression_time: Some(18),
                    decompression_time: Some(19),
                    total_page_compression_attempts: Some(20),
                    failed_page_compression_attempts: Some(21),
                    total_page_decompressions: Some(22),
                    compressed_page_evictions: Some(23),
                    eager_page_compressions: Some(24),
                    memory_pressure_page_compressions: Some(25),
                    critical_memory_page_compressions: Some(26),
                    pages_decompressed_unit_ns: Some(27),
                    pages_decompressed_within_log_time: Some([0, 1, 2, 3, 4, 5, 6, 7]),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };

        let output = process_snapshot(snapshot);

        assert_eq!(output.undigested, 0);
        assert_eq!(output.principals.len(), 4);

        let principals: HashMap<u64, output::PrincipalOutput> =
            output.principals.into_iter().map(|p| (p.id, p)).collect();

        assert_eq!(
            principals.get(&0).unwrap(),
            &output::PrincipalOutput {
                id: 0,
                name: "root".to_owned(),
                principal_type: "R".to_owned(),
                committed_private: 1024,
                committed_scaled: 1536.0,
                committed_total: 2048,
                populated_private: 2048,
                populated_scaled: 3072.0,
                populated_total: 4096,
                attributor: None,
                processes: vec!["root_process (1001)".to_owned()],
                vmos: vec![
                    (
                        "root_vmo".to_owned(),
                        output::VmoOutput {
                            count: 1,
                            committed_private: 1024,
                            committed_scaled: 1024.0,
                            committed_total: 1024,
                            populated_private: 2048,
                            populated_scaled: 2048.0,
                            populated_total: 2048,
                            ..Default::default()
                        }
                    ),
                    (
                        "shared_vmo".to_owned(),
                        output::VmoOutput {
                            count: 1,
                            committed_private: 0,
                            committed_scaled: 512.0,
                            committed_total: 1024,
                            populated_private: 0,
                            populated_scaled: 1024.0,
                            populated_total: 2048,
                            ..Default::default()
                        }
                    )
                ]
                .into_iter()
                .collect(),
            }
        );

        assert_eq!(
            principals.get(&1).unwrap(),
            &output::PrincipalOutput {
                id: 1,
                name: "runner".to_owned(),
                principal_type: "R".to_owned(),
                committed_private: 1024,
                committed_scaled: 1024.0,
                committed_total: 1024,
                populated_private: 2048,
                populated_scaled: 2048.0,
                populated_total: 2048,
                attributor: Some("root".to_owned()),
                processes: vec!["runner_process (1005)".to_owned()],
                vmos: vec![(
                    "runner_vmo".to_owned(),
                    output::VmoOutput {
                        count: 1,
                        committed_private: 1024,
                        committed_scaled: 1024.0,
                        committed_total: 1024,
                        populated_private: 2048,
                        populated_scaled: 2048.0,
                        populated_total: 2048,
                        ..Default::default()
                    }
                )]
                .into_iter()
                .collect(),
            }
        );

        assert_eq!(
            principals.get(&2).unwrap(),
            &output::PrincipalOutput {
                id: 2,
                name: "component 2".to_owned(),
                principal_type: "R".to_owned(),
                committed_private: 2048,
                committed_scaled: 2560.0,
                committed_total: 3072,
                populated_private: 4096,
                populated_scaled: 5120.0,
                populated_total: 6144,
                attributor: Some("root".to_owned()),
                processes: vec!["2_process (1009)".to_owned()],
                vmos: vec![
                    (
                        "shared_vmo".to_owned(),
                        output::VmoOutput {
                            count: 1,
                            committed_private: 0,
                            committed_scaled: 512.0,
                            committed_total: 1024,
                            populated_private: 0,
                            populated_scaled: 1024.0,
                            populated_total: 2048,
                            ..Default::default()
                        }
                    ),
                    (
                        "2_vmo".to_owned(),
                        output::VmoOutput {
                            count: 1,
                            committed_private: 1024,
                            committed_scaled: 1024.0,
                            committed_total: 1024,
                            populated_private: 2048,
                            populated_scaled: 2048.0,
                            populated_total: 2048,
                            ..Default::default()
                        }
                    ),
                    (
                        "2_vmo_parent".to_owned(),
                        output::VmoOutput {
                            count: 1,
                            committed_private: 1024,
                            committed_scaled: 1024.0,
                            committed_total: 1024,
                            populated_private: 2048,
                            populated_scaled: 2048.0,
                            populated_total: 2048,
                            ..Default::default()
                        }
                    )
                ]
                .into_iter()
                .collect(),
            }
        );

        assert_eq!(
            principals.get(&3).unwrap(),
            &output::PrincipalOutput {
                id: 3,
                name: "component 3".to_owned(),
                principal_type: "R".to_owned(),
                committed_private: 2176,
                committed_scaled: 2176.0,
                committed_total: 2176,
                populated_private: 4352,
                populated_scaled: 4352.0,
                populated_total: 4352,
                attributor: Some("runner".to_owned()),
                processes: vec!["runner_process (1005)".to_owned()],
                vmos: vec![
                    (
                        "component_vmo".to_owned(),
                        output::VmoOutput {
                            count: 1,
                            committed_private: 128,
                            committed_scaled: 128.0,
                            committed_total: 128,
                            populated_private: 256,
                            populated_scaled: 256.0,
                            populated_total: 256,
                            ..Default::default()
                        }
                    ),
                    (
                        "component_vmo_mapped".to_owned(),
                        output::VmoOutput {
                            count: 1,
                            committed_private: 1024,
                            committed_scaled: 1024.0,
                            committed_total: 1024,
                            populated_private: 2048,
                            populated_scaled: 2048.0,
                            populated_total: 2048,
                            ..Default::default()
                        }
                    ),
                    (
                        "component_vmo_mapped2".to_owned(),
                        output::VmoOutput {
                            count: 1,
                            committed_private: 1024,
                            committed_scaled: 1024.0,
                            committed_total: 1024,
                            populated_private: 2048,
                            populated_scaled: 2048.0,
                            populated_total: 2048,
                            ..Default::default()
                        }
                    )
                ]
                .into_iter()
                .collect(),
            }
        );
    }

    #[test]
    fn test_reshare_resources() {
        // Create a fake snapshot with 3 principals:
        // root (0)
        //  - component 1 (1)
        //    - component 2 (2)
        //
        // and the following job/process/vmo hierarchy:
        // root_job (1000)
        //  - component_job (1001)
        //    * component_process (1002)
        //      . component_vmo (1003)
        //
        // In this scenario, component 1 reattributes component_job to component 2 entirely.

        let snapshot = fplugin::Snapshot {
            attributions: Some(vec![
                fplugin::Attribution {
                    source: Some(fplugin::PrincipalIdentifier { id: 0 }),
                    subject: Some(fplugin::PrincipalIdentifier { id: 0 }),
                    resources: Some(vec![fplugin::ResourceReference::KernelObject(1000)]),
                    ..Default::default()
                },
                fplugin::Attribution {
                    source: Some(fplugin::PrincipalIdentifier { id: 0 }),
                    subject: Some(fplugin::PrincipalIdentifier { id: 1 }),
                    resources: Some(vec![fplugin::ResourceReference::KernelObject(1001)]),
                    ..Default::default()
                },
                fplugin::Attribution {
                    source: Some(fplugin::PrincipalIdentifier { id: 1 }),
                    subject: Some(fplugin::PrincipalIdentifier { id: 2 }),
                    resources: Some(vec![fplugin::ResourceReference::KernelObject(1001)]),
                    ..Default::default()
                },
            ]),
            principals: Some(vec![
                fplugin::Principal {
                    identifier: Some(fplugin::PrincipalIdentifier { id: 0 }),
                    description: Some(fplugin::Description::Component("root".to_owned())),
                    principal_type: Some(fplugin::PrincipalType::Runnable),
                    parent: None,
                    ..Default::default()
                },
                fplugin::Principal {
                    identifier: Some(fplugin::PrincipalIdentifier { id: 1 }),
                    description: Some(fplugin::Description::Component("component 1".to_owned())),
                    principal_type: Some(fplugin::PrincipalType::Runnable),
                    parent: Some(fplugin::PrincipalIdentifier { id: 0 }),
                    ..Default::default()
                },
                fplugin::Principal {
                    identifier: Some(fplugin::PrincipalIdentifier { id: 2 }),
                    description: Some(fplugin::Description::Component("component 2".to_owned())),
                    principal_type: Some(fplugin::PrincipalType::Runnable),
                    parent: Some(fplugin::PrincipalIdentifier { id: 1 }),
                    ..Default::default()
                },
            ]),
            resources: Some(vec![
                fplugin::Resource {
                    koid: Some(1000),
                    name_index: Some(0),
                    resource_type: Some(fplugin::ResourceType::Job(fplugin::Job {
                        child_jobs: Some(vec![1001]),
                        processes: Some(vec![]),
                        ..Default::default()
                    })),
                    ..Default::default()
                },
                fplugin::Resource {
                    koid: Some(1001),
                    name_index: Some(1),
                    resource_type: Some(fplugin::ResourceType::Job(fplugin::Job {
                        child_jobs: Some(vec![]),
                        processes: Some(vec![1002]),
                        ..Default::default()
                    })),
                    ..Default::default()
                },
                fplugin::Resource {
                    koid: Some(1002),
                    name_index: Some(2),
                    resource_type: Some(fplugin::ResourceType::Process(fplugin::Process {
                        vmos: Some(vec![1003]),
                        mappings: None,
                        ..Default::default()
                    })),
                    ..Default::default()
                },
                fplugin::Resource {
                    koid: Some(1003),
                    name_index: Some(3),
                    resource_type: Some(fplugin::ResourceType::Vmo(fplugin::Vmo {
                        committed_bytes: Some(1024),
                        populated_bytes: Some(2048),
                        ..Default::default()
                    })),
                    ..Default::default()
                },
            ]),
            resource_names: Some(vec![
                "root_job".to_owned(),
                "component_job".to_owned(),
                "component_process".to_owned(),
                "component_vmo".to_owned(),
            ]),
            kernel_statistics: Some(fplugin::KernelStatistics {
                memory_stats: Some(fidl_fuchsia_kernel::MemoryStats {
                    total_bytes: Some(1),
                    free_bytes: Some(2),
                    free_loaned_bytes: Some(3),
                    wired_bytes: Some(4),
                    total_heap_bytes: Some(5),
                    free_heap_bytes: Some(6),
                    vmo_bytes: Some(7),
                    mmu_overhead_bytes: Some(8),
                    ipc_bytes: Some(9),
                    cache_bytes: Some(10),
                    slab_bytes: Some(11),
                    zram_bytes: Some(12),
                    other_bytes: Some(13),
                    vmo_reclaim_total_bytes: Some(14),
                    vmo_reclaim_newest_bytes: Some(15),
                    vmo_reclaim_oldest_bytes: Some(16),
                    vmo_reclaim_disabled_bytes: Some(17),
                    vmo_discardable_locked_bytes: Some(18),
                    vmo_discardable_unlocked_bytes: Some(19),
                    ..Default::default()
                }),
                compression_stats: Some(fidl_fuchsia_kernel::MemoryStatsCompression {
                    uncompressed_storage_bytes: Some(15),
                    compressed_storage_bytes: Some(16),
                    compressed_fragmentation_bytes: Some(17),
                    compression_time: Some(18),
                    decompression_time: Some(19),
                    total_page_compression_attempts: Some(20),
                    failed_page_compression_attempts: Some(21),
                    total_page_decompressions: Some(22),
                    compressed_page_evictions: Some(23),
                    eager_page_compressions: Some(24),
                    memory_pressure_page_compressions: Some(25),
                    critical_memory_page_compressions: Some(26),
                    pages_decompressed_unit_ns: Some(27),
                    pages_decompressed_within_log_time: Some([0, 1, 2, 3, 4, 5, 6, 7]),
                    ..Default::default()
                }),
                ..Default::default()
            }),
            ..Default::default()
        };

        let output = process_snapshot(snapshot);

        assert_eq!(output.undigested, 0);
        assert_eq!(output.principals.len(), 3);

        let principals: HashMap<u64, output::PrincipalOutput> =
            output.principals.into_iter().map(|p| (p.id, p)).collect();

        assert_eq!(
            principals.get(&0).unwrap(),
            &output::PrincipalOutput {
                id: 0,
                name: "root".to_owned(),
                principal_type: "R".to_owned(),
                committed_private: 0,
                committed_scaled: 0.0,
                committed_total: 0,
                populated_private: 0,
                populated_scaled: 0.0,
                populated_total: 0,
                attributor: None,
                processes: vec![],
                vmos: vec![].into_iter().collect(),
            }
        );

        assert_eq!(
            principals.get(&1).unwrap(),
            &output::PrincipalOutput {
                id: 1,
                name: "component 1".to_owned(),
                principal_type: "R".to_owned(),
                committed_private: 0,
                committed_scaled: 0.0,
                committed_total: 0,
                populated_private: 0,
                populated_scaled: 0.0,
                populated_total: 0,
                attributor: Some("root".to_owned()),
                processes: vec![],
                vmos: vec![].into_iter().collect(),
            }
        );

        assert_eq!(
            principals.get(&2).unwrap(),
            &output::PrincipalOutput {
                id: 2,
                name: "component 2".to_owned(),
                principal_type: "R".to_owned(),
                committed_private: 1024,
                committed_scaled: 1024.0,
                committed_total: 1024,
                populated_private: 2048,
                populated_scaled: 2048.0,
                populated_total: 2048,
                attributor: Some("component 1".to_owned()),
                processes: vec!["component_process (1002)".to_owned()],
                vmos: vec![(
                    "component_vmo".to_owned(),
                    output::VmoOutput {
                        count: 1,
                        committed_private: 1024,
                        committed_scaled: 1024.0,
                        committed_total: 1024,
                        populated_private: 2048,
                        populated_scaled: 2048.0,
                        populated_total: 2048,
                        ..Default::default()
                    }
                ),]
                .into_iter()
                .collect(),
            }
        );
    }
}
