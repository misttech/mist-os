// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::cell::RefCell;
use core::convert::Into;
use fidl_fuchsia_memory_attribution_plugin as fplugin;
use futures::future::BoxFuture;
use std::collections::{HashMap, HashSet};
use summary::MemorySummary;

pub mod kernel_statistics;
pub mod summary;

#[derive(PartialEq, Eq, Hash, Clone, Copy, Debug)]
pub struct PrincipalIdentifier(pub u64);

impl From<fplugin::PrincipalIdentifier> for PrincipalIdentifier {
    fn from(value: fplugin::PrincipalIdentifier) -> Self {
        PrincipalIdentifier(value.id)
    }
}

impl Into<fplugin::PrincipalIdentifier> for PrincipalIdentifier {
    fn into(self) -> fplugin::PrincipalIdentifier {
        fplugin::PrincipalIdentifier { id: self.0 }
    }
}

/// User-understandable description of a Principal
#[derive(PartialEq, Eq, Clone, Debug, Hash)]
pub enum PrincipalDescription {
    Component(String),
    Part(String),
}

impl From<fplugin::Description> for PrincipalDescription {
    fn from(value: fplugin::Description) -> Self {
        match value {
            fplugin::Description::Component(s) => PrincipalDescription::Component(s),
            fplugin::Description::Part(s) => PrincipalDescription::Part(s),
            _ => unreachable!(),
        }
    }
}

impl Into<fplugin::Description> for PrincipalDescription {
    fn into(self) -> fplugin::Description {
        match self {
            PrincipalDescription::Component(s) => fplugin::Description::Component(s),
            PrincipalDescription::Part(s) => fplugin::Description::Part(s),
        }
    }
}

/// Type of a principal.
#[derive(PartialEq, Eq, Clone, Debug, Hash)]
pub enum PrincipalType {
    Runnable,
    Part,
}

impl From<fplugin::PrincipalType> for PrincipalType {
    fn from(value: fplugin::PrincipalType) -> Self {
        match value {
            fplugin::PrincipalType::Runnable => PrincipalType::Runnable,
            fplugin::PrincipalType::Part => PrincipalType::Part,
            _ => unreachable!(),
        }
    }
}

impl Into<fplugin::PrincipalType> for PrincipalType {
    fn into(self) -> fplugin::PrincipalType {
        match self {
            PrincipalType::Runnable => fplugin::PrincipalType::Runnable,
            PrincipalType::Part => fplugin::PrincipalType::Part,
        }
    }
}

#[derive(PartialEq, Eq, Debug, Hash)]
/// A Principal, that can use and claim memory.
pub struct Principal {
    // These fields are initialized from [fplugin::Principal].
    pub identifier: PrincipalIdentifier,
    pub description: PrincipalDescription,
    pub principal_type: PrincipalType,

    /// Principal that declared this Principal. None if this Principal is at the root of the system
    /// hierarchy (the root principal is a statically defined Principal encompassing all resources
    /// on the system). The Principal hierarchy forms a tree (no cycles).
    pub parent: Option<PrincipalIdentifier>,
}

/// Creates a new [Principal] from a [fplugin::Principal] object. The [Principal] object will
/// contain all the data from [fplugin::Principal] and have its other fields initialized empty.
impl From<fplugin::Principal> for Principal {
    fn from(value: fplugin::Principal) -> Self {
        Principal {
            identifier: value.identifier.unwrap().into(),
            description: value.description.unwrap().into(),
            principal_type: value.principal_type.unwrap().into(),
            parent: value.parent.map(Into::into),
        }
    }
}

impl Into<fplugin::Principal> for Principal {
    fn into(self) -> fplugin::Principal {
        fplugin::Principal {
            identifier: Some(self.identifier.into()),
            description: Some(self.description.into()),
            principal_type: Some(self.principal_type.into()),
            parent: self.parent.map(Into::into),
            ..Default::default()
        }
    }
}

/// A Principal, with its attribution claims and resources.
pub struct InflatedPrincipal {
    /// The principal definition.
    principal: Principal,

    // These fields are computed from the rest of the [fplugin::Snapshot] data.
    /// Map of attribution claims made about this Principal (this Principal is the subject of the
    /// claim). This map goes from the source Principal to the attribution claim.
    attribution_claims: HashMap<PrincipalIdentifier, Attribution>,
    /// KOIDs of resources attributed to this principal, after resolution of sharing and
    /// reattributions.
    resources: HashSet<u64>,
}

impl InflatedPrincipal {
    fn new(principal: Principal) -> InflatedPrincipal {
        InflatedPrincipal {
            principal,
            attribution_claims: Default::default(),
            resources: Default::default(),
        }
    }
}

impl InflatedPrincipal {
    fn name(&self) -> &str {
        match &self.principal.description {
            PrincipalDescription::Component(component_name) => component_name,
            PrincipalDescription::Part(part_name) => part_name,
        }
    }
}

/// Type of the claim, that changes depending on how the claim was created.
#[derive(Clone, Copy, Eq, PartialEq, Hash, Debug)]
pub enum ClaimType {
    /// A principal claimed this resource directly.
    Direct,
    /// A principal claimed a resource that contains this resource (e.g. a process containing a
    /// VMO).
    Indirect,
    /// A principal claimed a child of this resource (e.g. a copy-on-write VMO child of this VMO).
    Child,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Koid(u64);

impl From<u64> for Koid {
    fn from(value: u64) -> Self {
        Koid(value)
    }
}

/// Attribution claim of a Principal on a Resource.
///
/// Note that this object is slightly different from the [fplugin::Attribution] object: it goes from
/// a Resource to a Principal, and covers also indirect attribution claims of sub- and parent
/// resources.
#[derive(Clone, Copy, Eq, PartialEq, Hash, Debug)]
pub struct Claim {
    /// Principal to which the resources are attributed.
    subject: PrincipalIdentifier,
    /// Principal making the attribution claim.
    source: PrincipalIdentifier,
    claim_type: ClaimType,
}

#[derive(Debug, PartialEq)]
pub struct Resource {
    pub koid: u64,
    pub name_index: usize,
    pub resource_type: fplugin::ResourceType,
}

impl From<fplugin::Resource> for Resource {
    fn from(value: fplugin::Resource) -> Self {
        Resource {
            koid: value.koid.unwrap(),
            name_index: value.name_index.unwrap() as usize,
            resource_type: value.resource_type.unwrap(),
        }
    }
}

impl Into<fplugin::Resource> for Resource {
    fn into(self) -> fplugin::Resource {
        fplugin::Resource {
            koid: Some(self.koid),
            name_index: Some(self.name_index as u64),
            resource_type: Some(self.resource_type),
            ..Default::default()
        }
    }
}

// Claim with a boolean tag, to help find leaves in the claim assignment graph.
pub struct TaggedClaim(Claim, bool);

#[derive(Debug)]
pub struct InflatedResource {
    resource: Resource,
    claims: HashSet<Claim>,
}

impl InflatedResource {
    fn new(resource: Resource) -> InflatedResource {
        InflatedResource { resource, claims: Default::default() }
    }

    fn children(&self) -> Vec<u64> {
        match &self.resource.resource_type {
            fplugin::ResourceType::Job(job) => {
                let mut r: Vec<u64> = job.child_jobs.iter().flatten().map(|k| *k).collect();
                r.extend(job.processes.iter().flatten().map(|k| *k));
                if r.len() == 0 {
                    eprintln!("{} has no processes", self.resource.koid);
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
                    InflatedResource::process_claims_recursive(tagged_claim, &claims_by_source)
                        .into_iter(),
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
            leaves.append(&mut InflatedResource::process_claims_recursive(subject_claim, claims));
        }
        leaves
    }
}

#[derive(Clone)]
/// Holds the list of resources attributed to a Principal (subject) by another Principal (source).
pub struct Attribution {
    /// Principal making the attribution claim.
    pub source: PrincipalIdentifier,
    /// Principal to which the resources are attributed.
    pub subject: PrincipalIdentifier,
    /// List of resources attributed to `subject` by `source`.
    pub resources: Vec<ResourceReference>,
}

impl From<fplugin::Attribution> for Attribution {
    fn from(value: fplugin::Attribution) -> Attribution {
        Attribution {
            source: value.source.unwrap().into(),
            subject: value.subject.unwrap().into(),
            resources: value.resources.unwrap().into_iter().map(|r| r.into()).collect(),
        }
    }
}

impl Into<fplugin::Attribution> for Attribution {
    fn into(self) -> fplugin::Attribution {
        fplugin::Attribution {
            source: Some(self.source.into()),
            subject: Some(self.subject.into()),
            resources: Some(self.resources.into_iter().map(|r| r.into()).collect()),
            ..Default::default()
        }
    }
}

#[derive(Clone, Copy)]
/// References a kernel [`Resource`], or some subset of a [`Resource`] (such as a part of a process
/// address space).
pub enum ResourceReference {
    /// Identifies a kernel object whose memory is being attributed.
    ///
    /// Refers to all memory held by VMOs reachable from the object
    /// (currently a Job, Process or VMO).
    KernelObject(u64),

    /// Identifies a part of a process address space.
    ProcessMapped {
        /// The KOID of the process that this VMAR lives in.
        process: u64,

        /// Base address of the VMAR.
        base: u64,

        /// Length of the VMAR.
        len: u64,
    },
}

impl From<fplugin::ResourceReference> for ResourceReference {
    fn from(value: fplugin::ResourceReference) -> ResourceReference {
        match value {
            fidl_fuchsia_memory_attribution_plugin::ResourceReference::KernelObject(ko) => {
                ResourceReference::KernelObject(ko)
            }
            fidl_fuchsia_memory_attribution_plugin::ResourceReference::ProcessMapped(
                fplugin::ProcessMapped { process, base, len },
            ) => ResourceReference::ProcessMapped { process, base, len },
            _ => unimplemented!(),
        }
    }
}

impl Into<fplugin::ResourceReference> for ResourceReference {
    fn into(self) -> fplugin::ResourceReference {
        match self {
            ResourceReference::KernelObject(ko) => {
                fidl_fuchsia_memory_attribution_plugin::ResourceReference::KernelObject(ko)
            }
            ResourceReference::ProcessMapped { process, base, len } => {
                fidl_fuchsia_memory_attribution_plugin::ResourceReference::ProcessMapped(
                    fplugin::ProcessMapped { process, base, len },
                )
            }
        }
    }
}

/// Capture of the current memory usage of a device, as retrieved through the memory attribution
/// protocol. In this object, memory attribution is not resolved.
pub struct AttributionData {
    pub principals_vec: Vec<Principal>,
    pub resources_vec: Vec<Resource>,
    pub resource_names: Vec<String>,
    pub attributions: Vec<Attribution>,
}

pub trait AttributionDataProvider: Send + Sync {
    fn get_attribution_data(&self) -> BoxFuture<'_, Result<AttributionData, anyhow::Error>>;
}

/// Processed snapshot of the memory usage of a device, with attribution of memory resources to
/// Principals resolved.
pub struct ProcessedAttributionData {
    principals: HashMap<PrincipalIdentifier, RefCell<InflatedPrincipal>>,
    resources: HashMap<u64, RefCell<InflatedResource>>,
    resource_names: Vec<String>,
}

impl ProcessedAttributionData {
    fn new(
        principals: HashMap<PrincipalIdentifier, RefCell<InflatedPrincipal>>,
        resources: HashMap<u64, RefCell<InflatedResource>>,
        resource_names: Vec<String>,
    ) -> Self {
        Self { principals, resources, resource_names }
    }

    /// Create a summary view of the memory attribution_data. See [MemorySummary] for details.
    pub fn summary(&self) -> MemorySummary {
        MemorySummary::build(&self.principals, &self.resources, &self.resource_names)
    }
}

/// Process data from a [AttributionData] to resolve attribution claims.
pub fn attribute_vmos(attribution_data: AttributionData) -> ProcessedAttributionData {
    // Map from moniker token ID to Principal struct.
    let principals: HashMap<PrincipalIdentifier, RefCell<InflatedPrincipal>> = attribution_data
        .principals_vec
        .into_iter()
        .map(|p| (p.identifier.clone(), RefCell::new(InflatedPrincipal::new(p))))
        .collect();

    // Map from kernel resource koid to Resource struct.
    let mut resources: HashMap<u64, RefCell<InflatedResource>> = attribution_data
        .resources_vec
        .into_iter()
        .map(|r| (r.koid, RefCell::new(InflatedResource::new(r))))
        .collect();

    // Add direct claims to resources.
    for attribution in attribution_data.attributions {
        principals.get(&attribution.subject.clone().into()).map(|p| {
            p.borrow_mut().attribution_claims.insert(attribution.source.into(), attribution.clone())
        });
        for resource in attribution.resources {
            match resource {
                ResourceReference::KernelObject(koid) => {
                    if !resources.contains_key(&koid) {
                        continue;
                    }
                    resources.get_mut(&koid).unwrap().get_mut().claims.insert(Claim {
                        source: attribution.source.into(),
                        subject: attribution.subject.into(),
                        claim_type: ClaimType::Direct,
                    });
                }
                ResourceReference::ProcessMapped { process, base, len } => {
                    if !resources.contains_key(&process) {
                        continue;
                    }
                    let mut matched_vmos = Vec::new();
                    if let fplugin::ResourceType::Process(process_data) =
                        &resources.get(&process).unwrap().borrow().resource.resource_type
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
                                    source: attribution.source.into(),
                                    subject: attribution.subject.into(),
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
        if let fplugin::ResourceType::Vmo(vmo) = &resource.resource.resource_type {
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
                current_parent = match &current_resource.resource.resource_type {
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
        } else if let fplugin::ResourceType::Process(_) = &resource.resource.resource_type {
            for claim in &resource.claims {
                principals
                    .get(&claim.subject)
                    .unwrap()
                    .borrow_mut()
                    .resources
                    .insert(resource.resource.koid);
            }
        }
    }

    ProcessedAttributionData::new(principals, resources, attribution_data.resource_names)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;
    use fidl_fuchsia_memory_attribution_plugin as fplugin;
    use summary::{PrincipalSummary, VmoSummary};

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

        let resource_names = vec![
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
        ];

        let attributions = vec![
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
        ]
        .into_iter()
        .map(|a| a.into())
        .collect();

        let principals = vec![
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
        ]
        .into_iter()
        .map(|p| p.into())
        .collect();

        let resources = vec![
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
        ]
        .into_iter()
        .map(|r| r.into())
        .collect();

        let output = attribute_vmos(AttributionData {
            principals_vec: principals,
            resources_vec: resources,
            resource_names,
            attributions,
        })
        .summary();

        assert_eq!(output.undigested, 0);
        assert_eq!(output.principals.len(), 4);

        let principals: HashMap<u64, PrincipalSummary> =
            output.principals.into_iter().map(|p| (p.id, p)).collect();

        assert_eq!(
            principals.get(&0).unwrap(),
            &PrincipalSummary {
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
                        VmoSummary {
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
                        VmoSummary {
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
            &PrincipalSummary {
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
                    VmoSummary {
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
            &PrincipalSummary {
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
                        VmoSummary {
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
                        VmoSummary {
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
                        VmoSummary {
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
            &PrincipalSummary {
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
                        VmoSummary {
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
                        VmoSummary {
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
                        VmoSummary {
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

        let resource_names = vec![
            "root_job".to_owned(),
            "component_job".to_owned(),
            "component_process".to_owned(),
            "component_vmo".to_owned(),
        ];
        let attributions = vec![
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
        ]
        .into_iter()
        .map(|a| a.into())
        .collect();
        let principals = vec![
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
        ]
        .into_iter()
        .map(|p| p.into())
        .collect();

        let resources = vec![
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
        ]
        .into_iter()
        .map(|r| r.into())
        .collect();

        let output = attribute_vmos(AttributionData {
            principals_vec: principals,
            resources_vec: resources,
            resource_names,
            attributions,
        })
        .summary();

        assert_eq!(output.undigested, 0);
        assert_eq!(output.principals.len(), 3);

        let principals: HashMap<u64, PrincipalSummary> =
            output.principals.into_iter().map(|p| (p.id, p)).collect();

        assert_eq!(
            principals.get(&0).unwrap(),
            &PrincipalSummary {
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
            &PrincipalSummary {
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
            &PrincipalSummary {
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
                    VmoSummary {
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

    #[test]
    fn test_conversions() {
        let plugin_principal = fplugin::Principal {
            identifier: Some(fplugin::PrincipalIdentifier { id: 0 }),
            description: Some(fplugin::Description::Component("root".to_owned())),
            principal_type: Some(fplugin::PrincipalType::Runnable),
            parent: None,
            ..Default::default()
        };

        let data_principal: Principal = plugin_principal.clone().into();

        assert_eq!(plugin_principal, data_principal.into());

        let plugin_resources = vec![
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
                    ]),
                    ..Default::default()
                })),
                ..Default::default()
            },
        ];

        let data_resources: Vec<Resource> =
            plugin_resources.iter().cloned().map(|r| r.into()).collect();

        let actual_resources: Vec<fplugin::Resource> =
            data_resources.into_iter().map(|r| r.into()).collect();

        assert_eq!(plugin_resources, actual_resources);
    }
}
