// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    crate::indexer::*,
    crate::resolved_driver::{load_boot_driver, DriverPackageType, ResolvedDriver},
    anyhow::Context,
    fidl_fuchsia_component_resolution as fresolution, fidl_fuchsia_io as fio,
    serde::Deserialize,
    std::{collections::HashSet, rc::Rc},
};

#[derive(Deserialize)]
struct JsonDriver {
    driver_url: String,
}

fn log_error(err: anyhow::Error) -> anyhow::Error {
    tracing::error!("{:#?}", err);
    err
}

pub async fn load_boot_drivers(
    boot: &fio::DirectoryProxy,
    resolver: &fresolution::ResolverProxy,
    eager_drivers: &HashSet<url::Url>,
    disabled_drivers: &HashSet<url::Url>,
) -> Result<Vec<ResolvedDriver>, anyhow::Error> {
    let manifest = fuchsia_fs::directory::open_file_no_describe(
        &boot,
        "config/driver_index/boot_driver_manifest",
        fio::OpenFlags::RIGHT_READABLE,
    )
    .context("boot: Failed to open driver_manifest")?;
    let resolved_drivers = load_drivers(
        manifest,
        &resolver,
        &eager_drivers,
        &disabled_drivers,
        DriverPackageType::Boot,
    )
    .await
    .context("Error loading boot packages")
    .map_err(log_error)?;
    Ok(resolved_drivers)
}

pub async fn load_base_drivers(
    indexer: Rc<Indexer>,
    boot: &fio::DirectoryProxy,
    resolver: &fresolution::ResolverProxy,
    eager_drivers: &HashSet<url::Url>,
    disabled_drivers: &HashSet<url::Url>,
) -> Result<(), anyhow::Error> {
    let manifest = fuchsia_fs::directory::open_file_no_describe(
        &boot,
        "config/driver_index/base_driver_manifest",
        fio::OpenFlags::RIGHT_READABLE,
    )
    .context("boot: Failed to open driver_manifest")?;
    let resolved_drivers = load_drivers(
        manifest,
        &resolver,
        &eager_drivers,
        &disabled_drivers,
        DriverPackageType::Base,
    )
    .await
    .context("Error loading base packages")
    .map_err(log_error)?;
    for resolved_driver in &resolved_drivers {
        let mut composite_node_spec_manager = indexer.composite_node_spec_manager.borrow_mut();
        composite_node_spec_manager.new_driver_available(resolved_driver.clone());
    }
    indexer.load_base_repo(BaseRepo::Resolved(resolved_drivers));
    Ok(())
}

pub async fn load_drivers(
    manifest: fio::FileProxy,
    resolver: &fresolution::ResolverProxy,
    eager_drivers: &HashSet<url::Url>,
    disabled_drivers: &HashSet<url::Url>,
    package_type: DriverPackageType,
) -> Result<Vec<ResolvedDriver>, anyhow::Error> {
    let data: String = fuchsia_fs::file::read_to_string(&manifest)
        .await
        .context("Failed to read base manifest")?;
    let drivers: Vec<JsonDriver> = serde_json::from_str(&data)?;
    let mut resolved_drivers = std::vec::Vec::new();
    for driver in drivers {
        let url = match url::Url::parse(&driver.driver_url) {
            Ok(u) => u,
            Err(e) => {
                tracing::error!("Found bad driver url: {}: error: {}", driver.driver_url, e);
                continue;
            }
        };
        let resolve = ResolvedDriver::resolve(url, &resolver, package_type).await;
        if resolve.is_err() {
            continue;
        }

        let mut resolved_driver = resolve.unwrap();
        if disabled_drivers.contains(&resolved_driver.component_url) {
            tracing::info!("Skipping driver: {}", resolved_driver.component_url.to_string());
            continue;
        }
        tracing::info!("Found driver: {}", resolved_driver.component_url.to_string());
        if eager_drivers.contains(&resolved_driver.component_url) {
            resolved_driver.fallback = false;
        }
        resolved_drivers.push(resolved_driver);
    }
    Ok(resolved_drivers)
}

pub async fn load_unpackaged_boot_drivers(
    dir: &fio::DirectoryProxy,
    eager_drivers: &HashSet<url::Url>,
    disabled_drivers: &HashSet<url::Url>,
) -> Result<Vec<ResolvedDriver>, anyhow::Error> {
    let meta = fuchsia_fs::directory::open_directory_no_describe(
        &dir,
        "meta",
        fio::OpenFlags::RIGHT_READABLE,
    )?;

    let entries =
        fuchsia_fs::directory::readdir(&meta).await.context("boot: failed to read meta")?;

    let mut drivers = std::vec::Vec::new();
    for entry in entries
        .iter()
        .filter(|entry| entry.kind == fuchsia_fs::directory::DirentKind::File)
        .filter(|entry| entry.name.ends_with(".cm"))
    {
        let url_string = "fuchsia-boot:///#meta/".to_string() + &entry.name;
        let url = url::Url::parse(&url_string)?;
        let driver = load_boot_driver(&dir, url, DriverPackageType::Boot, None).await;
        if let Err(e) = driver {
            tracing::error!("Failed to load unpackaged boot driver: {}: {}", url_string, e);
            continue;
        }
        if let Ok(Some(mut driver)) = driver {
            if disabled_drivers.contains(&driver.component_url) {
                tracing::info!(
                    "Skipping unpackaged boot driver: {}",
                    driver.component_url.to_string()
                );
                continue;
            }
            tracing::info!("Found unpackaged boot driver: {}", driver.component_url.to_string());
            if eager_drivers.contains(&driver.component_url) {
                driver.fallback = false;
            }
            drivers.push(driver);
        }
    }
    Ok(drivers)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[fuchsia::test]
    async fn test_load_fallback_driver() {
        const DRIVER_URL: &str = "fuchsia-boot:///#meta/test-fallback-component.cm";
        let driver_url = url::Url::parse(&DRIVER_URL).unwrap();
        let pkg = fuchsia_fs::directory::open_in_namespace("/pkg", fio::OpenFlags::RIGHT_READABLE)
            .context("Failed to open /pkg")
            .unwrap();
        let fallback_driver = load_boot_driver(&pkg, driver_url, DriverPackageType::Boot, None)
            .await
            .unwrap()
            .expect("Fallback driver was not loaded");
        assert!(fallback_driver.fallback);
    }

    #[fuchsia::test]
    async fn test_load_eager_fallback_unpackaged_boot_driver() {
        let eager_driver_component_url =
            url::Url::parse("fuchsia-boot:///#meta/test-fallback-component.cm").unwrap();

        let boot = fuchsia_fs::directory::open_in_namespace("/pkg", fio::OpenFlags::RIGHT_READABLE)
            .unwrap();
        let drivers = load_unpackaged_boot_drivers(
            &boot,
            &HashSet::from([eager_driver_component_url.clone()]),
            &HashSet::new(),
        )
        .await
        .unwrap();
        assert!(
            !drivers
                .iter()
                .find(|driver| driver.component_url == eager_driver_component_url)
                .expect("Fallback driver did not load")
                .fallback
        );
    }
}
