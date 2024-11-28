// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::attribution_client::AttributionState;
use std::collections::{HashMap, HashSet};
use {
    fidl_fuchsia_memory_attribution as fattribution,
    fidl_fuchsia_memory_attribution_plugin as fplugin,
};

/// A structure containing a set of kernel resources (jobs, processes, VMOs), indexed by KOIDs.
#[derive(Default)]
pub struct KernelResources {
    /// Map of resource Koid to resource definition.
    pub resources: HashMap<zx::Koid, fplugin::Resource>,
    /// Map of resource name to unique identifier.
    ///
    /// Many different resources often share the same name. In order to minimize the space taken by
    /// resource definitions, we give each unique name an identifier, and refer to these
    /// identifiers in the resource definitions
    pub resource_names: HashMap<String, u64>,
    /// Value of the next resource identifier. It should be incremented each time a new name is
    /// inserted in `resource_names``.
    next_resource_name_index: u64,
}

/// Represents whether we should collect information about VMOs or memory maps of a process.
struct CollectionRequest {
    collect_vmos: bool,
    collect_maps: bool,
}

impl CollectionRequest {
    fn collect_vmos() -> Self {
        Self { collect_vmos: true, collect_maps: false }
    }

    fn collect_maps() -> Self {
        Self { collect_vmos: false, collect_maps: true }
    }

    fn merge(&mut self, other: &Self) {
        self.collect_vmos |= other.collect_vmos;
        self.collect_maps |= other.collect_maps;
    }
}

/// Interface for a Zircon job. This is useful to allow for dependency injection in tests.
pub trait Job {
    /// Returns the Koid of the job.
    fn get_koid(&self) -> Result<zx::Koid, zx::Status>;
    /// Returns the name of the job.
    fn get_name(&self) -> Result<zx::Name, zx::Status>;
    /// Returns the koids of the job children of the job.
    fn children(&self) -> Result<Vec<zx::Koid>, zx::Status>;
    /// Returns the koids of the processes directly held by this job.
    fn processes(&self) -> Result<Vec<zx::Koid>, zx::Status>;
    /// Return a child Job from its Koid.
    fn get_child_job(
        &self,
        koid: &zx::Koid,
        rights: zx::Rights,
    ) -> Result<Box<dyn Job>, zx::Status>;
    /// Returns a child Process from its Koid.
    fn get_child_process(
        &self,
        koid: &zx::Koid,
        rights: zx::Rights,
    ) -> Result<Box<dyn Process>, zx::Status>;
}

impl Job for zx::Job {
    fn get_koid(&self) -> Result<zx::Koid, zx::Status> {
        fidl::AsHandleRef::get_koid(&self)
    }

    fn get_name(&self) -> Result<zx::Name, zx::Status> {
        fidl::AsHandleRef::get_name(&self)
    }

    fn children(&self) -> Result<Vec<zx::Koid>, zx::Status> {
        zx::Job::children(&self)
    }

    fn processes(&self) -> Result<Vec<zx::Koid>, zx::Status> {
        zx::Job::processes(&self)
    }

    fn get_child_job(
        &self,
        koid: &zx::Koid,
        rights: zx::Rights,
    ) -> Result<Box<dyn Job>, zx::Status> {
        zx::Job::get_child(&self, koid, rights)
            .map(|handle| Box::<zx::Job>::new(handle.into()) as Box<dyn Job>)
    }

    fn get_child_process(
        &self,
        koid: &zx::Koid,
        rights: zx::Rights,
    ) -> Result<Box<dyn Process>, zx::Status> {
        zx::Job::get_child(&self, koid, rights)
            .map(|handle| Box::<zx::Process>::new(handle.into()) as Box<dyn Process>)
    }
}

/// Interface for a Zircon process. This is useful to allow for dependency injection in tests.
pub trait Process {
    /// Returns the name of the process.
    fn get_name(&self) -> Result<zx::Name, zx::Status>;
    /// Returns information about the VMOs accessible to this process.
    fn info_vmos_vec(&self) -> Result<Vec<zx::VmoInfo>, zx::Status>;
    /// Returns information about the memory mappings of this process.
    fn info_maps_vec(&self) -> Result<Vec<zx::MapInfo>, zx::Status>;
}

impl Process for zx::Process {
    fn get_name(&self) -> Result<zx::Name, zx::Status> {
        fidl::AsHandleRef::get_name(self)
    }

    fn info_vmos_vec(&self) -> Result<Vec<zx::VmoInfo>, zx::Status> {
        zx::Process::info_vmos_vec(self)
    }

    fn info_maps_vec(&self) -> Result<Vec<zx::MapInfo>, zx::Status> {
        zx::Process::info_maps_vec(self)
    }
}

impl KernelResources {
    pub fn get_resources(
        root: &Box<dyn Job>,
        attribution_state: &AttributionState,
    ) -> Result<KernelResources, zx::Status> {
        // For each process for which we have attribution information, decide what information we
        // need to collect.
        let claimed_resources_iterator =
            attribution_state.0.values().map(|p| p.resources.values().flatten()).flatten();

        // Now that we have an iterator over all claimed resources, we process each claim to know
        // what we need to collect.
        let process_collection_requests: HashMap<zx::Koid, CollectionRequest> =
            claimed_resources_iterator.fold(HashMap::new(), |mut hashmap, resource| {
                let (koid, resource_collection) = match resource {
                    fattribution::Resource::KernelObject(koid) => {
                        (zx::Koid::from_raw(*koid), CollectionRequest::collect_vmos())
                    }
                    fattribution::Resource::ProcessMapped(pm) => {
                        // Here, we assume that we would have learned about the VMOs elsewhere.
                        (zx::Koid::from_raw(pm.process), CollectionRequest::collect_maps())
                    }
                    fattribution::Resource::__SourceBreaking { unknown_ordinal: _ } => todo!(),
                };
                hashmap
                    .entry(koid)
                    .and_modify(|e| e.merge(&resource_collection))
                    .or_insert(resource_collection);
                hashmap
            });
        let mut kr = KernelResources::default();
        let root_job_koid = root.get_koid().unwrap();
        kr.explore_job(&root_job_koid, &root, &process_collection_requests)?;
        Ok(kr)
    }

    /// Recursively gather memory information from a job.
    fn explore_job(
        &mut self,
        koid: &zx::Koid,
        job: &Box<dyn Job>,
        process_mapped: &HashMap<zx::Koid, CollectionRequest>,
    ) -> Result<(), zx::Status> {
        let job_name = job.get_name()?.as_bstr().to_string();
        let child_jobs = job.children()?;
        let processes = job.processes()?;

        for child_job_koid in &child_jobs {
            // Here and below: jobs and processes can disappear while we explore the job
            // and process hierarchy. Therefore, we don't stop the exploration if we don't
            // find a previously mentioned job or process, but we just ignore it silently.
            let child_job = match job.get_child_job(child_job_koid, zx::Rights::SAME_RIGHTS) {
                Err(s) => {
                    if s == zx::Status::NOT_FOUND {
                        continue;
                    } else {
                        Err(s)?
                    }
                }
                Ok(child) => child,
            };
            self.explore_job(child_job_koid, &child_job, process_mapped)?;
        }

        for process_koid in &processes {
            let child_process = match job.get_child_process(process_koid, zx::Rights::SAME_RIGHTS) {
                Err(s) => {
                    if s == zx::Status::NOT_FOUND {
                        continue;
                    } else {
                        Err(s)?
                    }
                }
                Ok(child) => child,
            };
            match self.explore_process(
                process_koid,
                child_process,
                process_mapped.get(process_koid),
            ) {
                Err(s) => {
                    if s == zx::Status::NOT_FOUND {
                        continue;
                    } else {
                        Err(s)?
                    }
                }
                Ok(_) => continue,
            };
        }

        let name_index = self.ensure_resource_name(job_name);
        self.resources.insert(
            koid.clone(),
            fplugin::Resource {
                koid: Some(koid.raw_koid()),
                name_index: Some(name_index),
                resource_type: Some(fplugin::ResourceType::Job(fplugin::Job {
                    child_jobs: Some(child_jobs.iter().map(zx::Koid::raw_koid).collect()),
                    processes: Some(processes.iter().map(zx::Koid::raw_koid).collect()),
                    ..Default::default()
                })),
                ..Default::default()
            },
        );
        Ok(())
    }

    /// Ensures the resource name is registered and returns its index.
    fn ensure_resource_name(&mut self, resource_name: String) -> u64 {
        match self.resource_names.get(&resource_name) {
            Some(name_index) => *name_index,
            None => {
                let index = self.next_resource_name_index;
                self.resource_names.insert(resource_name, index);
                self.next_resource_name_index += 1;
                index
            }
        }
    }

    /// Gather the memory information of a process.
    fn explore_process(
        &mut self,
        koid: &zx::Koid,
        process: Box<dyn Process>,
        collection: Option<&CollectionRequest>,
    ) -> Result<(), zx::Status> {
        let process_name = process.get_name()?.as_bstr().to_string();

        let vmo_koids = if collection.is_none() || collection.is_some_and(|c| c.collect_vmos) {
            let info_vmos = process.info_vmos_vec()?;

            let mut vmo_koids = HashSet::with_capacity(info_vmos.len());
            for info_vmo in info_vmos {
                if !vmo_koids.insert(info_vmo.koid.clone()) {
                    // The VMO is already in the set, we can skip.
                    continue;
                }
                // No need to copy the VMO info if we have already seen it.
                if self.resources.contains_key(&info_vmo.koid) {
                    continue;
                }
                let name_index = self.ensure_resource_name(info_vmo.name.as_bstr().to_string());
                self.resources.insert(
                    info_vmo.koid.clone(),
                    fplugin::Resource {
                        koid: Some(info_vmo.koid.raw_koid()),
                        name_index: Some(name_index),
                        resource_type: Some(fplugin::ResourceType::Vmo(fplugin::Vmo {
                            committed_bytes: Some(info_vmo.committed_bytes),
                            populated_bytes: Some(info_vmo.populated_bytes),
                            parent: match info_vmo.parent_koid.raw_koid() {
                                0 => None,
                                k => Some(k),
                            },
                            ..Default::default()
                        })),
                        ..Default::default()
                    },
                );
            }
            Some(vmo_koids.iter().map(zx::Koid::raw_koid).collect())
        } else {
            None
        };

        let process_maps = if collection.is_some_and(|c| c.collect_maps) {
            let info_maps = process.info_maps_vec()?;

            let mut mappings = Vec::new();
            for info_map in info_maps {
                if let zx::MapDetails::Mapping(details) = info_map.details {
                    // SAFETY: the type is of type MAPPING, so the union contains the mapping information.
                    mappings.push(fplugin::Mapping {
                        vmo: Some(details.vmo_koid.raw_koid()),
                        address_base: Some(info_map.base.try_into().unwrap()),
                        size: Some(info_map.size.try_into().unwrap()),
                        ..Default::default()
                    });
                }
            }
            Some(mappings)
        } else {
            None
        };

        let name_index = self.ensure_resource_name(process_name);
        self.resources.insert(
            koid.clone(),
            fplugin::Resource {
                koid: Some(koid.raw_koid()),
                name_index: Some(name_index),
                resource_type: Some(fplugin::ResourceType::Process(fplugin::Process {
                    vmos: vmo_koids,
                    mappings: process_maps,
                    ..Default::default()
                })),
                ..Default::default()
            },
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use std::vec;

    use crate::attribution_client::{
        AttributionProvider, AttributionState, LocalPrincipalIdentifier,
    };
    use fidl_fuchsia_memory_attribution as fattribution;

    use super::*;

    #[derive(Clone)]
    struct FakeJob {
        koid: zx::Koid,
        name: zx::Name,
        children: HashMap<zx::Koid, FakeJob>,
        processes: HashMap<zx::Koid, FakeProcess>,
    }

    impl FakeJob {
        fn new(
            koid: u64,
            name: &str,
            children: Vec<FakeJob>,
            processes: Vec<FakeProcess>,
        ) -> FakeJob {
            FakeJob {
                koid: zx::Koid::from_raw(koid),
                name: zx::Name::from_bytes_lossy(name.as_bytes()),
                children: children.into_iter().map(|c| (c.koid, c)).collect(),
                processes: processes.into_iter().map(|p| (p.koid, p)).collect(),
            }
        }
    }

    impl Job for FakeJob {
        fn get_koid(&self) -> Result<zx::Koid, zx::Status> {
            Ok(self.koid)
        }

        fn get_name(&self) -> Result<zx::Name, zx::Status> {
            Ok(self.name.clone())
        }

        fn children(&self) -> Result<Vec<zx::Koid>, zx::Status> {
            Ok(self.children.keys().copied().collect())
        }

        fn processes(&self) -> Result<Vec<zx::Koid>, zx::Status> {
            Ok(self.processes.keys().copied().collect())
        }

        fn get_child_job(
            &self,
            koid: &zx::Koid,
            _rights: zx::Rights,
        ) -> Result<Box<dyn Job>, zx::Status> {
            Ok(Box::new(self.children.get(koid).ok_or(Err(zx::Status::NOT_FOUND))?.clone()))
        }

        fn get_child_process(
            &self,
            koid: &zx::Koid,
            _rights: zx::Rights,
        ) -> Result<Box<dyn Process>, zx::Status> {
            Ok(Box::new(self.processes.get(koid).ok_or(Err(zx::Status::NOT_FOUND))?.clone()))
        }
    }

    #[derive(Clone)]
    struct FakeProcess {
        koid: zx::Koid,
        name: zx::Name,
        vmos: Vec<zx::VmoInfo>,
        maps: Vec<zx::MapInfo>,
    }

    impl FakeProcess {
        fn new(
            koid: u64,
            name: &str,
            vmos: Vec<zx::VmoInfo>,
            maps: Vec<zx::MapInfo>,
        ) -> FakeProcess {
            FakeProcess {
                koid: zx::Koid::from_raw(koid),
                name: zx::Name::from_bytes_lossy(name.as_bytes()),
                vmos,
                maps,
            }
        }
    }

    impl Process for FakeProcess {
        fn get_name(&self) -> Result<zx::Name, zx::Status> {
            Ok(self.name.clone())
        }

        fn info_vmos_vec(&self) -> Result<Vec<zx::VmoInfo>, zx::Status> {
            Ok(self.vmos.clone())
        }

        fn info_maps_vec(&self) -> Result<Vec<zx::MapInfo>, zx::Status> {
            Ok(self.maps.clone())
        }
    }

    fn simple_vmo_info(
        koid: u64,
        name: &str,
        parent: u64,
        committed_bytes: u64,
        populated_bytes: u64,
    ) -> zx::VmoInfo {
        zx::VmoInfo {
            koid: zx::Koid::from_raw(koid),
            name: zx::Name::from_bytes_lossy(name.as_bytes()),
            size_bytes: populated_bytes,
            parent_koid: zx::Koid::from_raw(parent),
            num_children: 0,
            num_mappings: 0,
            share_count: 0,
            flags: zx::VmoInfoFlags::empty(),
            committed_bytes,
            handle_rights: zx::Rights::empty(),
            cache_policy: zx::CachePolicy::Unknown,
            metadata_bytes: 0,
            committed_change_events: 0,
            populated_bytes,
            committed_private_bytes: committed_bytes,
            populated_private_bytes: populated_bytes,
            committed_scaled_bytes: committed_bytes,
            populated_scaled_bytes: populated_bytes,
            committed_fractional_scaled_bytes: 0,
            populated_fractional_scaled_bytes: 0,
        }
    }

    #[test]
    fn test_gather_resources() {
        let root_job = Box::new(FakeJob::new(
            0,
            "root",
            vec![
                FakeJob::new(
                    1,
                    "job1",
                    vec![],
                    vec![FakeProcess::new(
                        11,
                        "proc11",
                        vec![
                            simple_vmo_info(111, "vmo111", 0, 100, 100),
                            simple_vmo_info(112, "vmo112", 0, 200, 200),
                        ],
                        vec![],
                    )],
                ),
                FakeJob::new(
                    2,
                    "job2",
                    vec![FakeJob::new(
                        3,
                        "job3",
                        vec![],
                        vec![FakeProcess::new(
                            31,
                            "proc31",
                            vec![],
                            vec![zx::MapInfo {
                                name: zx::Name::from_bytes_lossy("mapping31".as_bytes()),
                                base: 0x1200,
                                size: 1024,
                                depth: 2,
                                details: zx::MapDetails::Mapping(zx::MappingDetails {
                                    mmu_flags: zx::VmarFlagsExtended::PERM_READ,
                                    vmo_koid: zx::Koid::from_raw(211),
                                    vmo_offset: 0,
                                    committed_bytes: 100,
                                    populated_bytes: 100,
                                    committed_private_bytes: 100,
                                    populated_private_bytes: 100,
                                    committed_scaled_bytes: 100,
                                    populated_scaled_bytes: 100,
                                    committed_fractional_scaled_bytes: 0,
                                    populated_fractional_scaled_bytes: 0,
                                }),
                            }],
                        )],
                    )],
                    vec![FakeProcess::new(
                        21,
                        "proc21",
                        vec![simple_vmo_info(211, "vmo211", 0, 200, 200)],
                        vec![],
                    )],
                ),
            ],
            vec![],
        ));

        let mut attribution_state = AttributionState::default();
        let root_id = 1.into();
        attribution_state.0.insert(
            root_id,
            AttributionProvider {
                definitions: Default::default(),
                resources: vec![(
                    LocalPrincipalIdentifier::new_for_tests(1),
                    vec![fattribution::Resource::ProcessMapped(fattribution::ProcessMapped {
                        process: 31,
                        base: 0x1000,
                        len: 2048,
                    })],
                )]
                .into_iter()
                .collect(),
            },
        );
        let kernel_resoures =
            KernelResources::get_resources(&(root_job as Box<dyn Job>), &attribution_state)
                .expect("Failed to gather resources");

        if let fplugin::ResourceType::Process(proc11) = kernel_resoures
            .resources
            .get(&zx::Koid::from_raw(11))
            .expect("Unable to find proc11")
            .resource_type
            .as_ref()
            .expect("No resource type")
        {
            assert_eq!(proc11.vmos.as_ref().expect("No VMOs").len(), 2);
        } else {
            unreachable!("Not a process");
        }

        if let fplugin::ResourceType::Process(proc31) = kernel_resoures
            .resources
            .get(&zx::Koid::from_raw(31))
            .expect("Unable to find proc31")
            .resource_type
            .as_ref()
            .expect("No resource type")
        {
            assert_eq!(proc31.mappings.as_ref().expect("No mappings").len(), 1);
        } else {
            unreachable!("Not a process");
        }
    }
}
