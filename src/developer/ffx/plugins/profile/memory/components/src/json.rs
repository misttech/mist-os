// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use serde_json::{json, Value};
use {fidl_fuchsia_kernel as fkernel, fidl_fuchsia_memory_attribution_plugin as fplugin};

/// An object with this trait can be converted to, and from, JSON.
// TODO: https://fxbug.dev/369609539 - This trait is necessary as FIDL bindings don't derive serde's
// traits for serialization and deserialization.
pub(crate) trait JsonConvertible {
    fn to_json(&self) -> Value;
    fn from_json(value: &Value) -> Option<Self>
    where
        Self: std::marker::Sized;
}

impl JsonConvertible for fplugin::Snapshot {
    fn to_json(&self) -> Value {
        json!({
            "attributions": self.attributions.to_json(),
            "principals": self.principals.to_json(),
            "resources": self.resources.to_json(),
            "resource_names": self.resource_names.to_json(),
            "kernel_statistics": self.kernel_statistics.to_json(),
        })
    }

    fn from_json(value: &Value) -> Option<Self> {
        let Some(obj) = value.as_object() else {
            return None;
        };
        Some(Self {
            attributions: obj
                .get("attributions")
                .map(|o| Vec::<fplugin::Attribution>::from_json(o))
                .flatten(),
            principals: obj
                .get("principals")
                .map(|o| Vec::<fplugin::Principal>::from_json(o))
                .flatten(),
            resources: obj
                .get("resources")
                .map(|o| Vec::<fplugin::Resource>::from_json(o))
                .flatten(),
            resource_names: obj
                .get("resource_names")
                .map(|o| Vec::<String>::from_json(o))
                .flatten(),
            kernel_statistics: obj
                .get("kernel_statistics")
                .map(|o| fplugin::KernelStatistics::from_json(o))
                .flatten(),
            ..Default::default()
        })
    }
}

impl<T: JsonConvertible> JsonConvertible for Vec<T> {
    fn to_json(&self) -> Value {
        Value::Array(self.iter().map(|element| element.to_json()).collect())
    }

    fn from_json(value: &Value) -> Option<Self> {
        match value.as_array() {
            None => None,
            Some(arr) => Some(arr.iter().map(|v| T::from_json(v)).flatten().collect()),
        }
    }
}

impl<T: JsonConvertible> JsonConvertible for Option<T> {
    fn to_json(&self) -> Value {
        match self {
            None => Value::Null,
            Some(element) => json!(element.to_json()),
        }
    }

    fn from_json(value: &Value) -> Option<Self> {
        Some(T::from_json(value))
    }
}

impl<T: JsonConvertible, const U: usize> JsonConvertible for [T; U] {
    fn to_json(&self) -> Value {
        Value::Array(self.iter().map(|element| element.to_json()).collect())
    }

    fn from_json(value: &Value) -> Option<Self> {
        match value.as_array() {
            None => None,
            Some(arr) => {
                arr.iter().map(|v| T::from_json(v)).flatten().collect::<Vec<T>>().try_into().ok()
            }
        }
    }
}

impl JsonConvertible for i64 {
    fn to_json(&self) -> Value {
        json!(*self)
    }

    fn from_json(value: &Value) -> Option<Self> {
        value.as_i64()
    }
}

impl JsonConvertible for u64 {
    fn to_json(&self) -> Value {
        json!(*self)
    }

    fn from_json(value: &Value) -> Option<Self> {
        value.as_u64()
    }
}

impl JsonConvertible for String {
    fn to_json(&self) -> Value {
        json!(self)
    }

    fn from_json(value: &Value) -> Option<Self> {
        value.as_str().map(|s| s.to_owned())
    }
}

impl JsonConvertible for fplugin::PrincipalIdentifier {
    fn to_json(&self) -> Value {
        json!(self.id)
    }

    fn from_json(value: &Value) -> Option<Self> {
        value.as_u64().map(|v| fplugin::PrincipalIdentifier { id: v })
    }
}

impl JsonConvertible for fplugin::Attribution {
    fn to_json(&self) -> Value {
        json!([self.source.to_json(), self.subject.to_json(), self.resources.to_json()])
    }

    fn from_json(value: &Value) -> Option<Self> {
        value
            .as_array()
            .map(|arr| {
                if arr.len() != 3 {
                    None
                } else {
                    Some(fplugin::Attribution {
                        source: arr.get(0).map(fplugin::PrincipalIdentifier::from_json).flatten(),
                        subject: arr.get(1).map(fplugin::PrincipalIdentifier::from_json).flatten(),
                        resources: Vec::<fplugin::ResourceReference>::from_json(
                            arr.get(2).unwrap(),
                        ),
                        ..Default::default()
                    })
                }
            })
            .flatten()
    }
}

impl JsonConvertible for fplugin::ResourceReference {
    fn to_json(&self) -> Value {
        match self {
            fplugin::ResourceReference::KernelObject(koid) => json!({
                "kernel_object": koid
            }),
            fplugin::ResourceReference::ProcessMapped(fplugin::ProcessMapped {
                process,
                base,
                len,
            }) => json!({
                "process_mapped": {
                    "process": process,
                    "base": base,
                    "len": len,
                }
            }),
            fplugin::ResourceReference::__SourceBreaking { unknown_ordinal: _ } => unimplemented!(),
        }
    }

    fn from_json(value: &Value) -> Option<Self> {
        let Some(obj) = value.as_object() else { return None };
        if let Some(kernel_object) = obj.get("kernel_object") {
            return Some(fplugin::ResourceReference::KernelObject(kernel_object.as_u64().unwrap()));
        }
        if let Some(process_mapped) = obj.get("process_mapped") {
            let Some(process_obj) = process_mapped.as_object() else { return None };
            return Some(fplugin::ResourceReference::ProcessMapped(fplugin::ProcessMapped {
                process: process_obj.get("process").unwrap().as_u64().unwrap(),
                base: process_obj.get("base").unwrap().as_u64().unwrap(),
                len: process_obj.get("len").unwrap().as_u64().unwrap(),
            }));
        }
        None
    }
}

impl JsonConvertible for fplugin::Principal {
    fn to_json(&self) -> Value {
        json!([
            self.identifier.to_json(),
            self.description.to_json(),
            self.principal_type.to_json(),
            self.parent.to_json()
        ])
    }

    fn from_json(value: &Value) -> Option<Self> {
        let Some(obj) = value.as_array() else { return None };
        Some(Self {
            identifier: obj.get(0).map(fplugin::PrincipalIdentifier::from_json).flatten(),
            description: obj.get(1).map(|v| fplugin::Description::from_json(v)).flatten(),
            principal_type: obj.get(2).map(|v| fplugin::PrincipalType::from_json(v)).flatten(),
            parent: obj.get(3).map(fplugin::PrincipalIdentifier::from_json).flatten(),
            ..Default::default()
        })
    }
}

impl JsonConvertible for fplugin::Description {
    fn to_json(&self) -> Value {
        match self {
            fplugin::Description::Component(name) => json!({"component": name}),
            fplugin::Description::Part(name) => json!({"part": name}),
            fplugin::Description::__SourceBreaking { unknown_ordinal: _ } => unimplemented!(),
        }
    }

    fn from_json(value: &Value) -> Option<Self> {
        let Some(obj) = value.as_object() else { return None };
        if let Some(component_name) = obj.get("component") {
            return Some(fplugin::Description::Component(
                component_name.as_str().unwrap().to_owned(),
            ));
        }
        if let Some(part_name) = obj.get("part") {
            return Some(fplugin::Description::Part(part_name.as_str().unwrap().to_owned()));
        }
        None
    }
}

impl JsonConvertible for fplugin::PrincipalType {
    fn to_json(&self) -> Value {
        match self {
            fplugin::PrincipalType::Runnable => json!("runnable"),
            fplugin::PrincipalType::Part => json!("part"),
            fplugin::PrincipalType::__SourceBreaking { unknown_ordinal: _ } => unimplemented!(),
        }
    }

    fn from_json(value: &Value) -> Option<Self> {
        let Some(obj) = value.as_str() else { return None };
        match obj {
            "runnable" => Some(fplugin::PrincipalType::Runnable),
            "part" => Some(fplugin::PrincipalType::Runnable),
            _ => None,
        }
    }
}

impl JsonConvertible for fplugin::Resource {
    fn to_json(&self) -> Value {
        json!([self.koid, self.name_index, self.resource_type.to_json()])
    }

    fn from_json(value: &Value) -> Option<Self> {
        let Some(obj) = value.as_array() else { return None };
        Some(Self {
            koid: obj.get(0).map(|v| v.as_u64()).flatten(),
            name_index: obj.get(1).map(|v| v.as_u64()).flatten(),
            resource_type: obj.get(2).map(|v| fplugin::ResourceType::from_json(v)).flatten(),
            ..Default::default()
        })
    }
}

impl JsonConvertible for fplugin::ResourceType {
    fn to_json(&self) -> Value {
        match self {
            fplugin::ResourceType::Job(fplugin::Job {
                child_jobs,
                processes,
                __source_breaking: _,
            }) => json!({
                "job": [child_jobs.to_json(), processes.to_json()]
            }),
            fplugin::ResourceType::Process(fplugin::Process {
                vmos,
                mappings,
                __source_breaking: _,
            }) => json!({
                "process": [vmos.to_json(), mappings.to_json()],
            }),
            fplugin::ResourceType::Vmo(fplugin::Vmo {
                committed_bytes,
                populated_bytes,
                parent,
                __source_breaking: _,
            }) => json!({
                "vmo": [committed_bytes.to_json(), populated_bytes.to_json(), parent.to_json()],
            }),
            fplugin::ResourceType::__SourceBreaking { unknown_ordinal: _ } => unimplemented!(),
        }
    }

    fn from_json(value: &Value) -> Option<Self> {
        let Some(obj) = value.as_object() else { return None };
        if let Some(job) = obj.get("job") {
            let Some(arr) = job.as_array() else { return None };
            return Some(fplugin::ResourceType::Job(fplugin::Job {
                child_jobs: arr.get(0).map(|v| Vec::<u64>::from_json(v)).flatten(),
                processes: arr.get(1).map(|v| Vec::<u64>::from_json(v)).flatten(),
                ..Default::default()
            }));
        }
        if let Some(process) = obj.get("process") {
            let Some(arr) = process.as_array() else { return None };
            return Some(fplugin::ResourceType::Process(fplugin::Process {
                vmos: arr.get(0).map(|v| Vec::<u64>::from_json(v)).flatten(),
                mappings: arr.get(1).map(|v| Vec::<fplugin::Mapping>::from_json(v)).flatten(),
                ..Default::default()
            }));
        }
        if let Some(vmo) = obj.get("vmo") {
            let Some(arr) = vmo.as_array() else { return None };
            return Some(fplugin::ResourceType::Vmo(fplugin::Vmo {
                committed_bytes: arr.get(0).map(|v| u64::from_json(v)).flatten(),
                populated_bytes: arr.get(1).map(|v| u64::from_json(v)).flatten(),
                parent: arr.get(2).map(|v| u64::from_json(v)).flatten(),
                ..Default::default()
            }));
        }
        None
    }
}

impl JsonConvertible for fplugin::Mapping {
    fn to_json(&self) -> Value {
        json!([self.vmo.to_json(), self.address_base.to_json(), self.size.to_json(),])
    }

    fn from_json(value: &Value) -> Option<Self> {
        let Some(arr) = value.as_array() else { return None };
        Some(fplugin::Mapping {
            vmo: arr.get(0).map(|v| u64::from_json(v)).flatten(),
            address_base: arr.get(1).map(|v| u64::from_json(v)).flatten(),
            size: arr.get(2).map(|v| u64::from_json(v)).flatten(),
            ..Default::default()
        })
    }
}

impl JsonConvertible for fplugin::KernelStatistics {
    fn to_json(&self) -> Value {
        json!({
            "memory_stats": self.memory_stats.to_json(),
            "compression_stats": self.compression_stats.to_json(),
        })
    }

    fn from_json(value: &Value) -> Option<Self> {
        let Some(obj) = value.as_object() else {
            return None;
        };
        Some(Self {
            memory_stats: obj
                .get("memory_stats")
                .map(|v| fkernel::MemoryStatsExtended::from_json(v))
                .flatten(),
            compression_stats: obj
                .get("compression_stats")
                .map(|v| fkernel::MemoryStatsCompression::from_json(v))
                .flatten(),
            ..Default::default()
        })
    }
}

impl JsonConvertible for fkernel::MemoryStatsExtended {
    fn to_json(&self) -> Value {
        json!({
            "total_bytes": self.total_bytes.to_json(),
            "free_bytes": self.free_bytes.to_json(),
            "wired_bytes": self.wired_bytes.to_json(),
            "total_heap_bytes": self.total_heap_bytes.to_json(),
            "free_heap_bytes": self.free_heap_bytes.to_json(),
            "vmo_bytes": self.vmo_bytes.to_json(),
            "vmo_pager_total_bytes": self.vmo_pager_total_bytes.to_json(),
            "vmo_pager_newest_bytes": self.vmo_pager_newest_bytes.to_json(),
            "vmo_pager_oldest_bytes": self.vmo_pager_oldest_bytes.to_json(),
            "vmo_discardable_locked_bytes": self.vmo_discardable_locked_bytes.to_json(),
            "vmo_discardable_unlocked_bytes": self.vmo_discardable_unlocked_bytes.to_json(),
            "mmu_overhead_bytes": self.mmu_overhead_bytes.to_json(),
            "ipc_bytes": self.ipc_bytes.to_json(),
            "other_bytes": self.other_bytes.to_json(),
        })
    }

    fn from_json(value: &Value) -> Option<Self> {
        let Some(obj) = value.as_object() else {
            return None;
        };
        Some(Self {
            total_bytes: obj.get("total_bytes").map(|v| u64::from_json(v)).flatten(),
            free_bytes: obj.get("free_bytes").map(|v| u64::from_json(v)).flatten(),
            wired_bytes: obj.get("wired_bytes").map(|v| u64::from_json(v)).flatten(),
            total_heap_bytes: obj.get("total_heap_bytes").map(|v| u64::from_json(v)).flatten(),
            free_heap_bytes: obj.get("free_heap_bytes").map(|v| u64::from_json(v)).flatten(),
            vmo_bytes: obj.get("vmo_bytes").map(|v| u64::from_json(v)).flatten(),
            vmo_pager_total_bytes: obj
                .get("vmo_pager_total_bytes")
                .map(|v| u64::from_json(v))
                .flatten(),
            vmo_pager_newest_bytes: obj
                .get("vmo_pager_newest_bytes")
                .map(|v| u64::from_json(v))
                .flatten(),
            vmo_pager_oldest_bytes: obj
                .get("vmo_pager_oldest_bytes")
                .map(|v| u64::from_json(v))
                .flatten(),
            vmo_discardable_locked_bytes: obj
                .get("vmo_discardable_locked_bytes")
                .map(|v| u64::from_json(v))
                .flatten(),
            vmo_discardable_unlocked_bytes: obj
                .get("vmo_discardable_unlocked_bytes")
                .map(|v| u64::from_json(v))
                .flatten(),
            mmu_overhead_bytes: obj.get("mmu_overhead_bytes").map(|v| u64::from_json(v)).flatten(),
            ipc_bytes: obj.get("ipc_bytes").map(|v| u64::from_json(v)).flatten(),
            other_bytes: obj.get("other_bytes").map(|v| u64::from_json(v)).flatten(),
            ..Default::default()
        })
    }
}

impl JsonConvertible for fkernel::MemoryStatsCompression {
    fn to_json(&self) -> Value {
        json!({
        "uncompressed_storage_bytes": self.uncompressed_storage_bytes.to_json(),
        "compressed_storage_bytes": self.compressed_storage_bytes.to_json(),
        "compressed_fragmentation_bytes": self.compressed_fragmentation_bytes.to_json(),
        "compression_time": self.compression_time.to_json(),
        "decompression_time": self.decompression_time.to_json(),
        "total_page_compression_attempts": self.total_page_compression_attempts.to_json(),
        "failed_page_compression_attempts": self.failed_page_compression_attempts.to_json(),
        "total_page_decompressions": self.total_page_decompressions.to_json(),
        "compressed_page_evictions": self.compressed_page_evictions.to_json(),
        "eager_page_compressions": self.eager_page_compressions.to_json(),
        "memory_pressure_page_compressions": self.memory_pressure_page_compressions.to_json(),
        "critical_memory_page_compressions": self.critical_memory_page_compressions.to_json(),
        "pages_decompressed_unit_ns": self.pages_decompressed_unit_ns.to_json(),
        "pages_decompressed_within_log_time": self.pages_decompressed_within_log_time.to_json(),
        })
    }

    fn from_json(value: &Value) -> Option<Self> {
        let Some(obj) = value.as_object() else {
            return None;
        };
        Some(Self {
            uncompressed_storage_bytes: obj
                .get("uncompressed_storage_bytes")
                .map(|v| u64::from_json(v))
                .flatten(),
            compressed_storage_bytes: obj
                .get("compressed_storage_bytes")
                .map(|v| u64::from_json(v))
                .flatten(),
            compressed_fragmentation_bytes: obj
                .get("compressed_fragmentation_bytes")
                .map(|v| u64::from_json(v))
                .flatten(),
            compression_time: obj.get("compression_time").map(|v| i64::from_json(v)).flatten(),
            decompression_time: obj.get("decompression_time").map(|v| i64::from_json(v)).flatten(),
            total_page_compression_attempts: obj
                .get("total_page_compression_attempts")
                .map(|v| u64::from_json(v))
                .flatten(),
            failed_page_compression_attempts: obj
                .get("failed_page_compression_attempts")
                .map(|v| u64::from_json(v))
                .flatten(),
            total_page_decompressions: obj
                .get("total_page_decompressions")
                .map(|v| u64::from_json(v))
                .flatten(),
            compressed_page_evictions: obj
                .get("compressed_page_evictions")
                .map(|v| u64::from_json(v))
                .flatten(),
            eager_page_compressions: obj
                .get("eager_page_compressions")
                .map(|v| u64::from_json(v))
                .flatten(),
            memory_pressure_page_compressions: obj
                .get("memory_pressure_page_compressions")
                .map(|v| u64::from_json(v))
                .flatten(),
            critical_memory_page_compressions: obj
                .get("critical_memory_page_compressions")
                .map(|v| u64::from_json(v))
                .flatten(),
            pages_decompressed_unit_ns: obj
                .get("pages_decompressed_unit_ns")
                .map(|v| u64::from_json(v))
                .flatten(),
            pages_decompressed_within_log_time: obj
                .get("pages_decompressed_within_log_time")
                .map(|v| <[u64; 8]>::from_json(v))
                .flatten(),
            ..Default::default()
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::Context;
    use fidl_fuchsia_memory_attribution_plugin as fplugin;

    #[test]
    fn test_serialize_deserialize() {
        let example_snapshot = fplugin::Snapshot {
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
                    resources: Some(vec![fplugin::ResourceReference::KernelObject(1007)]),
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
                        vmos: Some(vec![1006, 1007]),
                        mappings: None,
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
                        committed_bytes: Some(1024),
                        populated_bytes: Some(2048),
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
            ]),
            kernel_statistics: Some(fplugin::KernelStatistics {
                memory_stats: Some(fidl_fuchsia_kernel::MemoryStatsExtended {
                    total_bytes: Some(1),
                    free_bytes: Some(2),
                    wired_bytes: Some(3),
                    total_heap_bytes: Some(4),
                    free_heap_bytes: Some(5),
                    vmo_bytes: Some(6),
                    vmo_pager_total_bytes: Some(7),
                    vmo_pager_newest_bytes: Some(8),
                    vmo_pager_oldest_bytes: Some(9),
                    vmo_discardable_locked_bytes: Some(10),
                    vmo_discardable_unlocked_bytes: Some(11),
                    mmu_overhead_bytes: Some(12),
                    ipc_bytes: Some(13),
                    other_bytes: Some(14),
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
        let json_value = example_snapshot.to_json();
        let actual_snapshot = fplugin::Snapshot::from_json(&json_value)
            .context("Unable to deserialize snapshot")
            .unwrap();
        assert_eq!(example_snapshot, actual_snapshot);
    }
}
