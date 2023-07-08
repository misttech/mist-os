// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::process::wait_for_process_termination,
    anyhow::Context as _,
    std::{fmt, fs::File, path::Path},
};

// TODO(fxbug.dev/129083): remove when fuchsia-pkg-testing stops using pm.
pub(crate) async fn generate_delivery_blob_in_path(
    uncompressed_blob_dir: impl AsRef<Path>,
    uncompressed_blob_path: impl fmt::Display,
    delivery_blob_dir: impl AsRef<Path>,
    delivery_blob_path: impl fmt::Display,
    blob_type: u32,
) -> Result<(), anyhow::Error> {
    let process = fdio::SpawnBuilder::new()
        .options(fdio::SpawnOptions::CLONE_ALL - fdio::SpawnOptions::CLONE_NAMESPACE)
        .arg("blobfs-compression")?
        .arg(format!("--source_file=/in/{uncompressed_blob_path}"))?
        .arg(format!("--compressed_file=/out/{delivery_blob_path}"))?
        .arg(format!("--type={blob_type}"))?
        .add_dir_to_namespace(
            "/in",
            File::open(uncompressed_blob_dir).context("open uncompressed_blob_dir")?,
        )?
        .add_dir_to_namespace(
            "/out",
            File::open(delivery_blob_dir).context("open delivery_blob_dir")?,
        )?
        .spawn_from_path("/pkg/bin/blobfs-compression", &fuchsia_runtime::job_default())
        .context("spawning blobfs-compression")?;
    wait_for_process_termination(process).await.context("waiting for blobfs-compression")
}

#[cfg(test)]
mod tests {
    use {super::*, fuchsia_async as fasync, std::fs};

    /// Given an uncompressed blob, generate a delivery blob.
    /// Requires /tmp directory in the namespace.
    /// See //src/storage/blobfs/delivery_blob.h for possible blob type values.
    async fn generate_delivery_blob(
        uncompressed_blob: impl AsRef<[u8]>,
        blob_type: u32,
    ) -> Result<Vec<u8>, anyhow::Error> {
        let tmp = tempfile::tempdir().context("create tmp")?;
        fs::write(tmp.path().join("uncompressed"), uncompressed_blob)
            .context("write uncompressed blob")?;
        generate_delivery_blob_in_path(&tmp, "uncompressed", &tmp, "delivery", blob_type).await?;
        fs::read(tmp.path().join("delivery")).context("read delivery blob")
    }

    #[fasync::run_singlethreaded(test)]
    async fn generate_delivery_blob_empty() {
        generate_delivery_blob([], 1).await.unwrap();
    }

    #[fasync::run_singlethreaded(test)]
    async fn generate_delivery_blob_small() {
        generate_delivery_blob([1; 234], 1).await.unwrap();
    }
}
