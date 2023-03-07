// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{Context, Result},
    fidl::endpoints::Proxy,
    fidl_fuchsia_driver_test as fdt, fidl_fuchsia_io as fio, fidl_fuchsia_rebind_test as frt,
    fuchsia_async as fasync,
    fuchsia_component_test::{RealmBuilder, RealmInstance},
    fuchsia_driver_test::{DriverTestRealmBuilder, DriverTestRealmInstance},
    fuchsia_fs::directory::{WatchEvent, Watcher},
    fuchsia_zircon as zx,
    futures::TryStreamExt,
    std::path::Path,
};

async fn start_driver_test_realm() -> Result<RealmInstance> {
    const ROOT_DRIVER_DFV2_URL: &str = "fuchsia-boot:///#meta/test-parent-sys.cm";

    let builder = RealmBuilder::new().await.context("Failed to create realm builder")?;
    builder.driver_test_realm_setup().await.context("Failed to setup driver test realm")?;
    let instance = builder.build().await.context("Failed to build realm instance")?;

    let mut realm_args = fdt::RealmArgs::EMPTY;
    realm_args.use_driver_framework_v2 = Some(true);
    realm_args.root_driver = Some(ROOT_DRIVER_DFV2_URL.to_owned());
    instance
        .driver_test_realm_start(realm_args)
        .await
        .context("Failed to start driver test realm")?;

    Ok(instance)
}

const PARENT_DEV_PATH: &str = "sys/test/rebind-parent";
const CHILD_DEV_PATH: &str = "sys/test/rebind-parent/rebind-child";

async fn wait_for_file_to_not_exist(dir: &fio::DirectoryProxy, name: &str) -> Result<()> {
    let mut watcher = Watcher::new(dir).await?;
    let mut exists = false;
    while let Some(msg) = watcher.try_next().await? {
        match msg.event {
            WatchEvent::REMOVE_FILE => {
                if msg.filename.to_str().unwrap() == name {
                    return Ok(());
                }
            }
            WatchEvent::EXISTING => {
                if msg.filename.to_str().unwrap() == name {
                    exists = true;
                }
            }
            WatchEvent::IDLE => {
                if !exists {
                    return Ok(());
                }
            }
            _ => {}
        }
    }
    anyhow::bail!("Watcher unexpectedly closed");
}

// Waits for a file found at `path` starting from `dir` to not exist. Assumes
// `dir` exists and the sub-directories of `path` exist.
async fn wait_for_path_to_not_exist(
    dir: &fio::DirectoryProxy,
    path: impl AsRef<Path>,
) -> Result<()> {
    let path = path.as_ref();
    let file_name = path
        .file_name()
        .ok_or(anyhow::anyhow!("Failed to get file name from path"))?
        .to_str()
        .ok_or(anyhow::anyhow!("Failed to convert path file name to string"))?;

    if let Some(sub_dir) = path.parent() {
        let dir = fuchsia_fs::directory::open_directory_no_describe(
            dir,
            sub_dir.to_str().ok_or(anyhow::anyhow!("Failed to convert path to string"))?,
            fio::OpenFlags::empty(),
        )?;
        wait_for_file_to_not_exist(&dir, file_name).await?;
    } else {
        wait_for_file_to_not_exist(dir, file_name).await?;
    }
    Ok(())
}

// Tests that a node will succesfully bind to a driver after the node has
// already been bound to that driver, then shutdown, then re-added.
#[fasync::run_singlethreaded(test)]
async fn test_rebind() -> Result<()> {
    let instance = start_driver_test_realm().await?;
    let dev = instance.driver_test_realm_connect_to_dev()?;

    let parent_node = device_watcher::recursive_wait_and_open_node(&dev, PARENT_DEV_PATH).await?;
    let parent = frt::RebindParentProxy::new(parent_node.into_channel().unwrap());
    parent.add_child().await?.map_err(|e| zx::Status::from_raw(e))?;
    device_watcher::recursive_wait_and_open_node(&dev, CHILD_DEV_PATH).await?;
    parent.remove_child().await?.map_err(|e| zx::Status::from_raw(e))?;
    wait_for_path_to_not_exist(&dev, CHILD_DEV_PATH).await?;
    parent.add_child().await?.map_err(|e| zx::Status::from_raw(e))?;
    device_watcher::recursive_wait_and_open_node(&dev, CHILD_DEV_PATH).await?;
    Ok(())
}
