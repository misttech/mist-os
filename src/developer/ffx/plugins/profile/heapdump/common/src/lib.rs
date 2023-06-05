// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use errors::ffx_bail;
use fidl_fuchsia_memory_heapdump_client as fheapdump_client;

mod pprof;
pub use crate::pprof::export_to_pprof;

mod realm_query;
pub use realm_query::connect_to_collector;

/// Builds a ProcessSelector value from command-line arguments.
pub fn build_process_selector(
    by_name: Option<String>,
    by_koid: Option<u64>,
) -> anyhow::Result<fheapdump_client::ProcessSelector> {
    match (by_name, by_koid) {
        (Some(selected_name), None) => Ok(fheapdump_client::ProcessSelector::ByName(selected_name)),
        (None, Some(selected_koid)) => Ok(fheapdump_client::ProcessSelector::ByKoid(selected_koid)),
        _ => anyhow::bail!("Please use either --by-name or --by-koid"),
    }
}

/// Converts a CollectorError into a user-friendly error.
///
/// The returned error is meant to be returned by the ffx plugin's main function.
pub fn check_collector_error<T>(
    value: Result<T, fheapdump_client::CollectorError>,
) -> anyhow::Result<T> {
    // Match known errors and return an FfxError (which is simply printed as-is).
    // For unknown errors, return a regular anyhow Error so that the full context with a "BUG"
    // banner is printed instead.
    match value {
        Ok(inner) => Ok(inner),
        Err(fheapdump_client::CollectorError::ProcessSelectorUnsupported) => {
            ffx_bail!("Unsupported process filter")
        }
        Err(fheapdump_client::CollectorError::ProcessSelectorNoMatch) => {
            ffx_bail!("No process matches the requested filter")
        }
        Err(fheapdump_client::CollectorError::ProcessSelectorAmbiguous) => {
            ffx_bail!("More than one process matches the requested filter")
        }
        Err(fheapdump_client::CollectorError::LiveSnapshotFailed) => {
            ffx_bail!("Failed to take a snapshot of the current live allocations in the process")
        }
        Err(fheapdump_client::CollectorError::StoredSnapshotNotFound) => {
            ffx_bail!("The requested snapshot ID does not exist")
        }
        Err(other) => {
            anyhow::bail!("Unrecognized CollectorError: {:?}", other)
        }
    }
}
