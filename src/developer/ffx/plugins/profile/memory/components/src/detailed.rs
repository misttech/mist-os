// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use attribution_processing::{
    digest, fplugin_serde, AttributionData, InflatedPrincipal, InflatedResource, Principal,
    ProcessedAttributionData, Resource, ZXName,
};
use fidl_fuchsia_memory_attribution_plugin::{self as fplugin};
use regex::bytes::Regex;
use serde::Serialize;

use crate::SnapshotAttributionDataProvider;

#[derive(Serialize)]
pub struct ComponentDetailedProfileResult {
    pub kernel: fplugin_serde::KernelStatistics,
    pub principals: Vec<InflatedPrincipal>,
    pub resources: Vec<InflatedResource>,
    pub resource_names: Vec<ZXName>,
    #[serde(with = "fplugin_serde::PerformanceImpactMetricsDef")]
    pub performance: fplugin::PerformanceImpactMetrics,
    pub digest: digest::Digest,
}

pub fn process_snapshot_detailed(
    snapshot: fplugin::Snapshot,
) -> Result<ComponentDetailedProfileResult> {
    // Map from moniker token ID to Principal struct.
    let principals: Vec<Principal> =
        snapshot.principals.into_iter().flatten().map(|p| p.into()).collect();

    // Map from kernel resource koid to Resource struct.
    let resources: Vec<Resource> =
        snapshot.resources.into_iter().flatten().map(|r| r.into()).collect();
    // Map from subject moniker token ID to Attribution struct.
    let attributions =
        snapshot.attributions.unwrap_or_default().into_iter().map(|a| a.into()).collect();
    let bucket_definitions: Vec<digest::BucketDefinition> = snapshot
        .bucket_definitions
        .as_ref()
        .unwrap_or(&Vec::new())
        .iter()
        .map(|bd| {
            let process = bd.process.as_ref().map(|p| Regex::new(&p)).transpose()?;
            let vmo = bd.vmo.as_ref().map(|p| Regex::new(&p)).transpose()?;
            Ok(digest::BucketDefinition {
                name: bd.name.clone().unwrap_or_default(),
                process,
                vmo,
                event_code: 0, // The information is unavailable client side.
            })
        })
        .collect::<Result<_>>()?;
    let digest = digest::Digest::compute(
        &SnapshotAttributionDataProvider {
            resources: &resources,
            resource_names: snapshot.resource_names.as_ref().unwrap_or(&Vec::new()),
        },
        snapshot
            .kernel_statistics
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Missing kernel statistics"))?
            .memory_stats
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Missing memory statistics"))?,
        snapshot
            .kernel_statistics
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Missing kernel statistics"))?
            .compression_stats
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("Missing compression statistics"))?,
        &bucket_definitions,
    )
    .expect("Digest computation should succeed");
    let ProcessedAttributionData { principals, resources, resource_names } =
        attribution_processing::attribute_vmos(AttributionData {
            principals_vec: principals,
            resources_vec: resources,
            resource_names: snapshot
                .resource_names
                .unwrap_or_default()
                .iter()
                .map(|n| ZXName::from_bytes_lossy(n))
                .collect(),
            attributions,
        });
    Ok(ComponentDetailedProfileResult {
        kernel: snapshot.kernel_statistics.unwrap_or_default().into(),
        principals: principals.into_values().map(|p| p.into_inner()).collect(),
        resources: resources.into_values().map(|r| r.into_inner()).collect(),
        resource_names,
        digest,
        performance: snapshot.performance_metrics.unwrap_or_default(),
    })
}
