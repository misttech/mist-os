// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::base_packages::{BasePackages, CachePackages},
    crate::index::PackageIndex,
    anyhow::Context as _,
    fidl_fuchsia_space::{
        ErrorCode as SpaceErrorCode, ManagerRequest as SpaceManagerRequest,
        ManagerRequestStream as SpaceManagerRequestStream,
    },
    fidl_fuchsia_update::CommitStatusProviderProxy,
    fuchsia_zircon::{self as zx, AsHandleRef},
    futures::prelude::*,
    std::sync::Arc,
    tracing::{error, info},
};

pub async fn serve(
    blobfs: blobfs::Client,
    base_packages: Arc<BasePackages>,
    cache_packages: Arc<CachePackages>,
    package_index: Arc<async_lock::RwLock<PackageIndex>>,
    commit_status_provider: CommitStatusProviderProxy,
    mut stream: SpaceManagerRequestStream,
    cache_package_protection: super::CachePackageProtection,
) -> Result<(), anyhow::Error> {
    let event_pair = commit_status_provider
        .is_current_system_committed()
        .await
        .context("while getting event pair")?;

    while let Some(event) = stream.try_next().await? {
        let SpaceManagerRequest::Gc { responder } = event;
        responder.send(
            gc(
                &blobfs,
                base_packages.as_ref(),
                cache_packages.as_ref(),
                &package_index,
                &event_pair,
                cache_package_protection,
            )
            .await,
        )?;
    }
    Ok(())
}

async fn gc(
    blobfs: &blobfs::Client,
    base_packages: &BasePackages,
    cache_packages: &CachePackages,
    package_index: &Arc<async_lock::RwLock<PackageIndex>>,
    event_pair: &zx::EventPair,
    cache_package_protection: super::CachePackageProtection,
) -> Result<(), SpaceErrorCode> {
    info!("performing gc");

    event_pair.wait_handle(zx::Signals::USER_0, zx::Time::INFINITE_PAST).map_err(|e| {
        match e {
            zx::Status::TIMED_OUT => {
                info!("GC is blocked pending update.");
            }
            zx::Status::CANCELED => {
                info!("Commit handle is closed, likely because we are rebooting.");
            }
            other => {
                error!("Got unexpected status {:?} while waiting on handle.", other);
            }
        }
        SpaceErrorCode::PendingCommit
    })?;

    async move {
        // Determine all resident blobs before locking the package index to decrease the amount of
        // time the package index lock is held. Blobs written after this are implicitly protected.
        // This is for speed and not necessary for correctness. It would still be correct if the
        // list of resident blobs was determined any time after the package index lock is taken.
        let mut eligible_blobs = blobfs.list_known_blobs().await?;

        // Lock the package index until we are done deleting blobs. During resolution, required
        // blobs are added to the index before their presence in blobfs is checked, so locking
        // the index until we are done deleting blobs guarantees we will never delete a blob
        // that resolution thinks it doesn't need to fetch.
        let package_index = package_index.read().await;
        package_index.all_blobs().iter().for_each(|blob| {
            eligible_blobs.remove(blob);
        });

        base_packages.list_blobs().iter().for_each(|blob| {
            eligible_blobs.remove(blob);
        });

        use super::CachePackageProtection::*;
        match cache_package_protection {
            Always => cache_packages.list_blobs().iter().for_each(|blob| {
                eligible_blobs.remove(blob);
            }),
            TreatLikeRegular => (),
        }

        info!("Garbage collecting {} blobs...", eligible_blobs.len());
        for (i, blob) in eligible_blobs.iter().enumerate() {
            blobfs.delete_blob(blob).await?;
            if (i + 1) % 100 == 0 {
                info!("{} blobs collected...", i + 1);
            }
        }
        info!("Garbage collection done. Collected {} blobs.", eligible_blobs.len());
        Ok(())
    }
    .await
    .map_err(|e: anyhow::Error| {
        error!("Failed to perform GC operation: {:#}", e);
        SpaceErrorCode::Internal
    })
}
