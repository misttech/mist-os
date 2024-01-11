// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

mod test_utils;

use {
    anyhow::{Context, Result},
    bind_fuchsia_platform, fidl_fuchsia_driver_development as fdd,
    fidl_fuchsia_driver_framework as fdf, fidl_fuchsia_driver_test as fdt,
    fuchsia_async::{self as fasync},
    fuchsia_component_test::{RealmBuilder, RealmInstance},
    fuchsia_driver_test::{DriverTestRealmBuilder, DriverTestRealmInstance},
    fuchsia_zircon_status as zx_status,
};

const SAMPLE_DRIVER_URL: &str = "fuchsia-boot:///#meta/sample-driver.cm";
const PARENT_DRIVER_URL: &str = "fuchsia-boot:///#meta/test-parent-sys.cm";
const FAKE_DRIVER_URL: &str = "fuchsia-boot:///#meta/driver-test-realm-fake-driver.cm";

fn get_no_protocol_dfv2_property_list() -> Option<[fdf::NodeProperty; 2]> {
    Some([
        fdf::NodeProperty {
            key: fdf::NodePropertyKey::IntValue(bind::ddk_bind_constants::BIND_PROTOCOL),
            value: fdf::NodePropertyValue::IntValue(28), // ZX_PROTOCOL_MISC
        },
        fdf::NodeProperty {
            key: fdf::NodePropertyKey::StringValue(String::from(
                bind_fuchsia_platform::DRIVER_FRAMEWORK_VERSION,
            )),
            value: fdf::NodePropertyValue::IntValue(2),
        },
    ])
}

fn get_test_parent_dfv2_property_list() -> Option<[fdf::NodeProperty; 2]> {
    Some([
        fdf::NodeProperty {
            key: fdf::NodePropertyKey::IntValue(bind::ddk_bind_constants::BIND_PROTOCOL),
            value: fdf::NodePropertyValue::IntValue(bind_fuchsia_test::BIND_PROTOCOL_PARENT),
        },
        fdf::NodeProperty {
            key: fdf::NodePropertyKey::StringValue(String::from(
                bind_fuchsia_platform::DRIVER_FRAMEWORK_VERSION,
            )),
            value: fdf::NodePropertyValue::IntValue(2),
        },
    ])
}

fn assert_not_found_error(error: fidl::Error) {
    if let fidl::Error::ClientChannelClosed { status, protocol_name: _ } = error {
        assert_eq!(status, zx_status::Status::NOT_FOUND);
    } else {
        panic!("Expcted ClientChannelClosed error");
    }
}

fn send_get_device_info_request(
    service: &fdd::ManagerProxy,
    device_filter: &[&str],
    exact_match: bool,
) -> Result<fdd::NodeInfoIteratorProxy> {
    let (iterator, iterator_server) =
        fidl::endpoints::create_proxy::<fdd::NodeInfoIteratorMarker>()?;

    service
        .get_node_info(
            &device_filter.iter().copied().map(String::from).collect::<Vec<_>>(),
            iterator_server,
            exact_match,
        )
        .context("FIDL call to get device info failed")?;

    Ok(iterator)
}

async fn get_device_info(
    service: &fdd::ManagerProxy,
    device_filter: &[&str],
    exact_match: bool,
) -> Result<Vec<fdd::NodeInfo>> {
    let iterator = send_get_device_info_request(service, device_filter, exact_match)?;

    let mut device_infos = Vec::new();
    loop {
        let mut device_info =
            iterator.get_next().await.context("FIDL call to get device info failed")?;
        if device_info.len() == 0 {
            break;
        }
        device_infos.append(&mut device_info);
    }
    Ok(device_infos)
}

fn send_get_driver_info_request(
    service: &fdd::ManagerProxy,
    driver_filter: &[&str],
) -> Result<fdd::DriverInfoIteratorProxy> {
    let (iterator, iterator_server) =
        fidl::endpoints::create_proxy::<fdd::DriverInfoIteratorMarker>()?;

    service
        .get_driver_info(
            &driver_filter.iter().copied().map(String::from).collect::<Vec<_>>(),
            iterator_server,
        )
        .context("FIDL call to get driver info failed")?;

    Ok(iterator)
}

async fn get_driver_info(
    service: &fdd::ManagerProxy,
    driver_filter: &[&str],
) -> Result<Vec<fdf::DriverInfo>> {
    let iterator = send_get_driver_info_request(service, driver_filter)?;

    let mut driver_infos = Vec::new();
    loop {
        let mut driver_info =
            iterator.get_next().await.context("FIDL call to get driver info failed")?;
        if driver_info.len() == 0 {
            break;
        }
        driver_infos.append(&mut driver_info)
    }
    Ok(driver_infos)
}

async fn set_up_test_driver_realm(use_dfv2: bool) -> Result<(RealmInstance, fdd::ManagerProxy)> {
    const ROOT_DRIVER_DFV2_URL: &str = PARENT_DRIVER_URL;

    let builder = RealmBuilder::new().await?;
    builder.driver_test_realm_setup().await?;
    let instance = builder.build().await?;

    let mut realm_args = fdt::RealmArgs::default();
    realm_args.use_driver_framework_v2 = Some(use_dfv2);
    if use_dfv2 {
        // DriverTestRealm attempts to bind the .cm of test-parent-sys if not explicitly requested otherwise.
        realm_args.root_driver = Some(ROOT_DRIVER_DFV2_URL.to_owned());
    }
    instance.driver_test_realm_start(realm_args).await?;

    let driver_dev = instance.root.connect_to_protocol_at_exposed_dir::<fdd::ManagerMarker>()?;

    // Make sure we wait until all the drivers are bound before returning.
    let dev = instance.driver_test_realm_connect_to_dev()?;
    device_watcher::recursive_wait(&dev, "sys/test/sample_driver")
        .await
        .expect("recursive wait failed");

    Ok((instance, driver_dev))
}

fn assert_contains_driver_url(driver_infos: &Vec<fdf::DriverInfo>, expected_driver_url: &str) {
    assert!(driver_infos
        .iter()
        .find(|driver_info| driver_info.url.as_ref().expect("Missing device URL")
            == expected_driver_url)
        .is_some());
}

// GetDriverInfo tests
// DFv1
#[fasync::run_singlethreaded(test)]
async fn test_get_driver_info_no_filter_dfv1() -> Result<()> {
    let (_instance, driver_dev) = set_up_test_driver_realm(false).await?;
    let driver_infos = get_driver_info(&driver_dev, &[]).await?;

    assert_eq!(driver_infos.len(), 3);
    assert_contains_driver_url(&driver_infos, SAMPLE_DRIVER_URL);
    assert_contains_driver_url(&driver_infos, PARENT_DRIVER_URL);
    assert_contains_driver_url(&driver_infos, FAKE_DRIVER_URL);
    Ok(())
}

#[fasync::run_singlethreaded(test)]
async fn test_get_driver_info_with_filter_dfv1() -> Result<()> {
    const DRIVER_FILTER: [&str; 1] = [SAMPLE_DRIVER_URL];

    let (_instance, driver_dev) = set_up_test_driver_realm(false).await?;
    let driver_infos = get_driver_info(&driver_dev, &DRIVER_FILTER).await?;

    assert_eq!(driver_infos.len(), 1);
    assert_contains_driver_url(&driver_infos, SAMPLE_DRIVER_URL);
    Ok(())
}

#[fasync::run_singlethreaded(test)]
async fn test_get_driver_info_with_mixed_filter_dfv1() -> Result<()> {
    const DRIVER_FILTER: [&str; 2] = ["fuchsia-boot:///#driver/sample-driver.so", "foo"];

    let (_instance, driver_dev) = set_up_test_driver_realm(false).await?;
    let iterator = send_get_driver_info_request(&driver_dev, &DRIVER_FILTER)?;
    let res = iterator.get_next().await.expect_err("A driver should not be returned");

    assert_not_found_error(res);
    Ok(())
}

#[fasync::run_singlethreaded(test)]
async fn test_get_driver_info_with_incomplete_filter_dfv1() -> Result<()> {
    const DRIVER_FILTER: [&str; 1] = ["fuchsia-boot:///#driver/sample-driver"];

    let (_instance, driver_dev) = set_up_test_driver_realm(false).await?;
    let iterator = send_get_driver_info_request(&driver_dev, &DRIVER_FILTER)?;
    let res = iterator.get_next().await.expect_err("A driver should not be returned");

    assert_not_found_error(res);
    Ok(())
}

#[fasync::run_singlethreaded(test)]
async fn test_get_driver_info_not_found_filter_dfv1() -> Result<()> {
    const DRIVER_FILTER: [&str; 1] = ["foo"];

    let (_instance, driver_dev) = set_up_test_driver_realm(false).await?;
    let iterator = send_get_driver_info_request(&driver_dev, &DRIVER_FILTER)?;
    let res = iterator.get_next().await.expect_err("A driver should not be returned");

    assert_not_found_error(res);
    Ok(())
}

// DFv2
#[fasync::run_singlethreaded(test)]
async fn test_get_driver_info_no_filter_dfv2() -> Result<()> {
    let (_instance, driver_dev) = set_up_test_driver_realm(true).await?;
    let driver_infos = get_driver_info(&driver_dev, &[]).await?;

    assert_eq!(driver_infos.len(), 3);
    assert_contains_driver_url(&driver_infos, SAMPLE_DRIVER_URL);
    assert_contains_driver_url(&driver_infos, PARENT_DRIVER_URL);
    assert_contains_driver_url(&driver_infos, FAKE_DRIVER_URL);
    Ok(())
}

#[fasync::run_singlethreaded(test)]
async fn test_get_driver_info_with_filter_dfv2() -> Result<()> {
    const DRIVER_FILTER: [&str; 1] = [SAMPLE_DRIVER_URL];

    let (_instance, driver_dev) = set_up_test_driver_realm(true).await?;
    let driver_infos = get_driver_info(&driver_dev, &DRIVER_FILTER).await?;

    assert_eq!(driver_infos.len(), 1);
    assert_contains_driver_url(&driver_infos, SAMPLE_DRIVER_URL);
    Ok(())
}

#[fasync::run_singlethreaded(test)]
async fn test_get_driver_info_with_duplicate_filter_dfv2() -> Result<()> {
    const DRIVER_FILTER: [&str; 2] = [SAMPLE_DRIVER_URL, SAMPLE_DRIVER_URL];

    let (_instance, driver_dev) = set_up_test_driver_realm(true).await?;
    let driver_infos = get_driver_info(&driver_dev, &DRIVER_FILTER).await?;

    assert_eq!(driver_infos.len(), 1);
    assert_contains_driver_url(&driver_infos, SAMPLE_DRIVER_URL);

    Ok(())
}

#[fasync::run_singlethreaded(test)]
async fn test_get_driver_info_with_mixed_filter_dfv2() -> Result<()> {
    const DRIVER_FILTER: [&str; 2] = [SAMPLE_DRIVER_URL, "foo"];

    let (_instance, driver_dev) = set_up_test_driver_realm(true).await?;
    let driver_infos = get_driver_info(&driver_dev, &DRIVER_FILTER).await?;

    assert_eq!(driver_infos.len(), 1);
    assert_contains_driver_url(&driver_infos, SAMPLE_DRIVER_URL);

    Ok(())
}

#[fasync::run_singlethreaded(test)]
async fn test_get_driver_info_with_incomplete_filter_dfv2() -> Result<()> {
    const DRIVER_FILTER: [&str; 1] = ["fuchsia-boot:///#meta/sample-driver"];

    let (_instance, driver_dev) = set_up_test_driver_realm(true).await?;
    let iterator = send_get_driver_info_request(&driver_dev, &DRIVER_FILTER)?;
    let res = iterator.get_next().await.expect_err("A driver should not be returned");

    assert_not_found_error(res);
    Ok(())
}

#[fasync::run_singlethreaded(test)]
async fn test_get_driver_info_not_found_filter_dfv2() -> Result<()> {
    const DRIVER_FILTER: [&str; 1] = ["foo"];

    let (_instance, driver_dev) = set_up_test_driver_realm(true).await?;
    let iterator = send_get_driver_info_request(&driver_dev, &DRIVER_FILTER)?;
    let res = iterator.get_next().await.expect_err("A driver should not be returned");

    assert_not_found_error(res);
    Ok(())
}

// TODO(b/316176095): Re-enable test after ensuring it works with DFv2.
#[ignore]
#[fasync::run_singlethreaded(test)]
async fn test_get_device_info_fuzzy_filter() -> Result<()> {
    const DEVICE_FILTER: [&str; 1] = ["sample"];

    let (_instance, driver_dev) = set_up_test_driver_realm(false).await?;
    let device_infos =
        get_device_info(&driver_dev, &DEVICE_FILTER, /* exact_match= */ false).await?;
    assert_eq!(device_infos.len(), 1);

    let device_nodes = test_utils::create_device_topology(device_infos);
    assert_eq!(device_nodes.len(), 1);

    let matched_node = &device_nodes[0];
    let Some(fdd::VersionedNodeInfo::V1(matched_node_info)) = &matched_node.info.versioned_info
    else {
        panic!("wrong info type");
    };
    assert_eq!(
        matched_node_info.topological_path.as_ref().expect("DFv1 device missing topological path"),
        "/dev/sys/test/sample_driver"
    );

    Ok(())
}

// DFv2
#[fasync::run_singlethreaded(test)]
async fn test_get_device_info_no_filter_dfv2() -> Result<()> {
    let (_instance, driver_dev) = set_up_test_driver_realm(true).await?;
    let device_infos = get_device_info(&driver_dev, &[], /* exact_match= */ true).await?;

    let device_nodes = test_utils::create_device_topology(device_infos);
    assert_eq!(device_nodes.len(), 1);

    let root = &device_nodes[0];
    let Some(fdd::VersionedNodeInfo::V2(root_info)) = &root.info.versioned_info else {
        panic!("wrong info type");
    };

    assert_eq!(root_info.moniker.as_ref().expect("DFv2 node missing moniker"), "dev");
    assert_eq!(
        root.info.bound_driver_url.as_ref().expect("DFv2 node missing driver URL"),
        PARENT_DRIVER_URL
    );
    assert!(root_info.node_property_list.is_none());
    assert_eq!(root.num_children, 1);
    assert_eq!(root.child_nodes.len(), 1);

    let sys = &root.child_nodes[0];
    let Some(fdd::VersionedNodeInfo::V2(sys_info)) = &sys.info.versioned_info else {
        panic!("wrong info type");
    };

    assert_eq!(sys_info.moniker.as_ref().expect("DFv2 node missing moniker"), "dev.sys");
    assert_eq!(
        sys.info.bound_driver_url.as_ref().expect("DFv2 node missing driver URL"),
        "unbound"
    );
    assert_eq!(
        sys_info.node_property_list.as_ref().map(|x| x.as_slice()),
        get_no_protocol_dfv2_property_list().as_ref().map(|x| x.as_slice())
    );
    assert_eq!(sys.num_children, 1);
    assert_eq!(sys.child_nodes.len(), 1);

    let test = &sys.child_nodes[0];
    let Some(fdd::VersionedNodeInfo::V2(test_info)) = &test.info.versioned_info else {
        panic!("wrong info type");
    };

    assert_eq!(test_info.moniker.as_ref().expect("DFv2 node missing moniker"), "dev.sys.test");
    assert_eq!(
        test.info.bound_driver_url.as_ref().expect("DFv2 node missing driver URL"),
        SAMPLE_DRIVER_URL
    );
    assert_eq!(
        test_info.node_property_list.as_ref().map(|x| x.as_slice()),
        get_test_parent_dfv2_property_list().as_ref().map(|x| x.as_slice())
    );
    Ok(())
}

#[fasync::run_singlethreaded(test)]
async fn test_get_device_info_with_filter_dfv2() -> Result<()> {
    const DEVICE_FILTER: [&str; 1] = ["dev.sys.test"];

    let (_instance, driver_dev) = set_up_test_driver_realm(true).await?;
    let device_infos =
        get_device_info(&driver_dev, &DEVICE_FILTER, /* exact_match= */ true).await?;

    let device_nodes = test_utils::create_device_topology(device_infos);
    assert_eq!(device_nodes.len(), 1);

    let root_sys_test = &device_nodes[0];
    let Some(fdd::VersionedNodeInfo::V2(root_sys_test_info)) = &root_sys_test.info.versioned_info
    else {
        panic!("wrong info type");
    };

    assert_eq!(
        root_sys_test_info.moniker.as_ref().expect("DFv2 node missing moniker"),
        "dev.sys.test"
    );
    assert_eq!(
        root_sys_test.info.bound_driver_url.as_ref().expect("DFv2 node missing driver URL"),
        SAMPLE_DRIVER_URL
    );
    assert_eq!(
        root_sys_test_info.node_property_list.as_ref().map(|x| x.as_slice()),
        get_test_parent_dfv2_property_list().as_ref().map(|x| x.as_slice())
    );
    assert!(root_sys_test.child_nodes.is_empty());
    Ok(())
}

#[fasync::run_singlethreaded(test)]
async fn test_get_device_info_with_duplicate_filter_dfv2() -> Result<()> {
    const DEVICE_FILTER: [&str; 2] = ["dev.sys.test", "dev.sys.test"];

    let (_instance, driver_dev) = set_up_test_driver_realm(true).await?;
    let device_infos =
        get_device_info(&driver_dev, &DEVICE_FILTER, /* exact_match= */ true).await?;

    let device_nodes = test_utils::create_device_topology(device_infos);
    assert_eq!(device_nodes.len(), 1);

    let root_sys_test = &device_nodes[0];
    let Some(fdd::VersionedNodeInfo::V2(root_sys_test_info)) = &root_sys_test.info.versioned_info
    else {
        panic!("wrong info type");
    };

    assert_eq!(
        root_sys_test_info.moniker.as_ref().expect("DFv2 node missing moniker"),
        "dev.sys.test"
    );
    assert_eq!(
        root_sys_test.info.bound_driver_url.as_ref().expect("DFv2 node missing driver URL"),
        SAMPLE_DRIVER_URL
    );
    assert_eq!(
        root_sys_test_info.node_property_list.as_ref().map(|x| x.as_slice()),
        get_test_parent_dfv2_property_list().as_ref().map(|x| x.as_slice())
    );
    assert!(root_sys_test.child_nodes.is_empty());
    Ok(())
}

#[fasync::run_singlethreaded(test)]
async fn test_get_device_info_with_incomplete_filter_dfv2() -> Result<()> {
    const DEVICE_FILTER: [&str; 1] = ["dev.sys.te"];

    let (_instance, driver_dev) = set_up_test_driver_realm(true).await?;
    let device_infos =
        get_device_info(&driver_dev, &DEVICE_FILTER, /* exact_match= */ true).await?;

    assert!(device_infos.is_empty());
    Ok(())
}

#[fasync::run_singlethreaded(test)]
async fn test_get_device_info_not_found_filter_dfv2() -> Result<()> {
    const DEVICE_FILTER: [&str; 1] = ["foo"];

    let (_instance, driver_dev) = set_up_test_driver_realm(true).await?;
    let device_infos =
        get_device_info(&driver_dev, &DEVICE_FILTER, /* exact_match= */ true).await?;

    assert!(device_infos.is_empty());
    Ok(())
}

#[fasync::run_singlethreaded(test)]
async fn test_add_test_node_dfv2() -> Result<()> {
    let (instance, driver_dev) = set_up_test_driver_realm(true).await?;

    driver_dev
        .add_test_node(&fdd::TestNodeAddArgs {
            name: Some("test_sample".to_string()),
            properties: Some(vec![fdf::NodeProperty {
                key: fdf::NodePropertyKey::IntValue(bind::ddk_bind_constants::BIND_PROTOCOL),
                value: fdf::NodePropertyValue::IntValue(bind_fuchsia_test::BIND_PROTOCOL_PARENT),
            }]),
            ..Default::default()
        })
        .await?
        .unwrap();

    let dev = instance.driver_test_realm_connect_to_dev()?;
    device_watcher::recursive_wait(&dev, "test_sample/sample_driver").await?;
    driver_dev.remove_test_node("test_sample").await?.unwrap();
    assert_eq!(
        Err(zx_status::Status::NOT_FOUND.into_raw()),
        driver_dev.remove_test_node("test_sample").await?
    );

    Ok(())
}
