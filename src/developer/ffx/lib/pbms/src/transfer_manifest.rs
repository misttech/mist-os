// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Download files referenced in a Transfer Manifest JSON file.
//!
//! This builds upon the lower level /src/lib/transfer_manifest lib.

use {
    crate::{
        pbms::{fetch_from_url, GS_SCHEME},
        string_from_url, AuthFlowChoice,
    },
    ::gcs::{
        client::{Client, ProgressResult, ProgressState},
        gs_url::split_gs_url,
    },
    ::transfer_manifest::TransferManifest,
    anyhow::{bail, Context, Result},
    futures::{StreamExt as _, TryStreamExt as _},
    std::{
        format,
        path::{Component, Path, PathBuf},
    },
    structured_ui,
};

/// Download a set of files referenced in the `transfer_manifest_url`.
///
/// Files will be nested under `local_dir` which must exist when this function
/// is called.
pub async fn transfer_download<F, I>(
    transfer_manifest_url: &url::Url,
    local_dir: &Path,
    auth_flow: &AuthFlowChoice,
    progress: &F,
    ui: &I,
    client: &Client,
) -> Result<()>
where
    F: Fn(Vec<ProgressState<'_>>) -> ProgressResult,
    I: structured_ui::Interface + Sync,
{
    let start = std::time::Instant::now();
    tracing::debug!(
        "transfer_download, transfer_manifest_url {:?}, local_dir {:?}",
        transfer_manifest_url,
        local_dir
    );
    assert!(local_dir.is_dir());

    let tm = string_from_url(&transfer_manifest_url, auth_flow, &|f| progress(vec![f]), ui, client)
        .await
        .with_context(|| format!("string from gcs: {:?}", transfer_manifest_url))?;

    let manifest = serde_json::from_str::<TransferManifest>(&tm)
        .with_context(|| format!("Parsing json {:?}", tm))?;
    match &manifest {
        TransferManifest::V1(v1_data) => transfer_download_v1(
            transfer_manifest_url,
            v1_data,
            local_dir,
            auth_flow,
            progress,
            ui,
            client,
        )
        .await
        .context("transferring from v1 manifest")?,
    }
    tracing::debug!("Total fetch images runtime {} seconds.", start.elapsed().as_secs_f32());
    Ok(())
}

/// Join `relative` onto `base` with light path normalization.
/// Errors out if the final path is not within `base`.
fn safe_join(base: &Path, relative: &Path) -> Result<PathBuf> {
    let mut normalized_relative = PathBuf::new();
    for part in relative.components() {
        match part {
            Component::Prefix(_) | Component::RootDir | Component::CurDir => {}
            Component::ParentDir => {
                if !normalized_relative.pop() {
                    tracing::warn!("Failed to normalize path: {}", relative.to_string_lossy());
                    break;
                }
            }
            Component::Normal(part_str) => normalized_relative.push(part_str),
        }
    }

    if normalized_relative.parent().is_some() {
        Ok(base.join(normalized_relative))
    } else {
        bail!(
            "Cannot safely concat \"{}\" onto \"{}\"",
            relative.to_string_lossy(),
            base.to_string_lossy()
        )
    }
}

/// Helper for transfer_download specifically for version 1 transfer manifests.
///
/// Files will be nested under `local_dir` which must exist when this function
/// is called.
async fn transfer_download_v1<F, I>(
    transfer_manifest_url: &url::Url,
    transfer_manifest: &transfer_manifest::TransferManifestV1,
    local_dir: &Path,
    auth_flow: &AuthFlowChoice,
    progress: &F,
    ui: &I,
    client: &Client,
) -> Result<()>
where
    F: Fn(Vec<ProgressState<'_>>) -> ProgressResult,
    I: structured_ui::Interface + Sync,
{
    fn malformed_warning<T>(err: T) -> T {
        eprintln!("The specified remote transfer manifest is malformed or broken.");
        eprintln!(
            "This can happen when you're trying to download a broken or unsupported build/release."
        );
        eprintln!("Please try a different product bundle transfer url.");
        err
    }

    let base_url = match transfer_manifest_url.scheme() {
        GS_SCHEME => format!(
            "gs://{}",
            split_gs_url(&transfer_manifest_url.as_str())
                .context("splitting transfer_manifest_url")?
                .0
        ),
        _ => transfer_manifest_url[..url::Position::BeforePath].to_string(),
    };
    let mut tasks = Vec::new();
    let transfer_entry_count = transfer_manifest.entries.len() as u64;
    for (i, transfer_entry) in transfer_manifest.entries.iter().enumerate() {
        // Avoid using base_url.join().
        let te_remote_dir = format!("{}/{}", base_url, transfer_entry.remote.as_str());

        let te_local_dir = safe_join(&local_dir, transfer_entry.local.as_std_path())
            .context("parsing path: `entries[].local`")
            .map_err(malformed_warning)?;
        let artifact_entry_count = transfer_entry.entries.len() as u64;
        for (k, artifact_entry) in transfer_entry.entries.iter().enumerate() {
            // Avoid using te_remote_dir.join().
            let remote_file =
                url::Url::parse(&format!("{}/{}", te_remote_dir, artifact_entry.name.as_str()))?;

            let local_file = safe_join(&te_local_dir, artifact_entry.name.as_std_path())
                .context("parsing path: `entries[].entries[].name`")
                .map_err(malformed_warning)?;
            let local_parent = local_file.parent().context("getting local parent")?.to_path_buf();
            async_fs::create_dir_all(&local_parent)
                .await
                .with_context(|| format!("creating local_parent {:?}", local_parent))?;

            tracing::debug!("Transfer {:?} to {:?}", remote_file, local_parent);
            tasks.push(async move {
                fetch_from_url(
                    &remote_file,
                    local_parent,
                    auth_flow,
                    &|_, f| {
                        // The directory progress is replaced because a transfer
                        // manifest refers to specific files to copy (not
                        // directories at time), so the directory progress is
                        // always "1 of 1 files", which is not helpful.
                        let section = ProgressState {
                            name: &transfer_entry.remote.as_str(),
                            at: i as u64 + 1,
                            of: transfer_entry_count,
                            units: "sections",
                        };
                        let directory = ProgressState {
                            name: &remote_file.as_str(),
                            at: k as u64,
                            of: artifact_entry_count,
                            units: "files",
                        };
                        progress(vec![section, directory, f])
                    },
                    ui,
                    &client.clone(),
                )
                .await
            })
        }
    }
    let mut stream = futures::stream::iter(tasks.into_iter())
        .buffer_unordered(std::thread::available_parallelism()?.get());

    while let Some(()) = stream.try_next().await? {}
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_safe_join() -> Result<()> {
        macro_rules! assert_joined {
            ($base:literal, $relative:literal, $result:literal) => {
                assert_eq!(safe_join(Path::new($base), Path::new($relative))?, Path::new($result));
            };
        }

        macro_rules! assert_cannot_join {
            ($base:literal, $relative:literal) => {
                assert_eq!(
                    safe_join(Path::new($base), Path::new($relative)).unwrap_err().to_string(),
                    concat!("Cannot safely concat \"", $relative, "\" onto \"", $base, "\""),
                );
            };
        }

        // absolute / relative
        assert_joined!("/absolute", "relative", "/absolute/relative");

        // relative / relative
        assert_joined!("./relative", "./subdir", "./relative/subdir");
        assert_joined!("relative", "subdir", "relative/subdir");

        // Join with subdirs.
        assert_joined!("/absolute/subdir", "relative/subdir", "/absolute/subdir/relative/subdir");

        // Second path is normalized.
        assert_joined!(
            "/absolute/subdir/..",
            "relative/subdir/../subdir",
            "/absolute/subdir/../relative/subdir"
        );

        // Second path is treated as relative
        assert_joined!("/absolute", "/not_absolute", "/absolute/not_absolute");

        // Subpath must be non-empty.
        assert_cannot_join!("./relative", "");
        assert_cannot_join!("./relative", ".");
        assert_cannot_join!("./relative", "./subdir/..");

        // Subpath cannot escape the base directory.
        assert_cannot_join!("/absolute/path", "..");
        assert_cannot_join!("/absolute/path", "/../path");
        assert_cannot_join!("/absolute/path", "a_dir/../..");

        Ok(())
    }
}
