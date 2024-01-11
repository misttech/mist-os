// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    anyhow::{anyhow, format_err, Context, Result},
    fidl_fuchsia_driver_development as fdd, fidl_fuchsia_driver_framework as fdf, futures,
};

#[derive(Debug)]
pub struct DFv1Device(pub fdd::NodeInfo);

impl DFv1Device {
    /// Gets the full topological path name of the device.
    pub fn get_topo_path(&self) -> Result<&str> {
        let topological_path = self.0.versioned_info.as_ref().and_then(|info| match info {
            fdd::VersionedNodeInfo::V1(v1) => v1.topological_path.as_ref(),
            _ => None,
        });
        Ok(topological_path.ok_or(format_err!("Missing topological path"))?)
    }

    /// Gets the last ordinal of the device's topological path.
    ///
    /// For a `topological_path` value of "/some/topo/path/foo/bar", "bar" will be returned.
    pub fn extract_name(&self) -> Result<&str> {
        let topological_path = self.get_topo_path()?;
        let (_, name) = topological_path.rsplit_once('/').unwrap_or(("", &topological_path));
        Ok(name)
    }

    pub fn get_v1_info(&self) -> Result<&fdd::V1DeviceInfo> {
        let info = self.0.versioned_info.as_ref().and_then(|info| match info {
            fdd::VersionedNodeInfo::V1(v1) => Some(v1),
            _ => None,
        });

        Ok(info.ok_or(format_err!("Missing v1 info"))?)
    }
}

#[derive(Debug)]
pub struct DFv2Node(pub fdd::NodeInfo);

impl DFv2Node {
    /// Gets the full moniker name of the device.
    pub fn get_moniker(&self) -> Result<&str> {
        let moniker = self.0.versioned_info.as_ref().and_then(|info| match info {
            fdd::VersionedNodeInfo::V2(v2) => v2.moniker.as_ref(),
            _ => None,
        });
        Ok(moniker.ok_or(format_err!("Missing moniker"))?)
    }

    /// Gets the last ordinal of the device's moniker.
    ///
    /// For a `moniker` value of "this.is.a.moniker.foo.bar", "bar" will be returned.
    pub fn extract_name(&self) -> Result<&str> {
        let moniker = self.get_moniker()?;
        let (_, name) = moniker.rsplit_once('.').unwrap_or(("", &moniker));
        Ok(name)
    }

    pub fn get_v2_info(&self) -> Result<&fdd::V2NodeInfo> {
        let info = self.0.versioned_info.as_ref().and_then(|info| match info {
            fdd::VersionedNodeInfo::V2(v2) => Some(v2),
            _ => None,
        });

        Ok(info.ok_or(format_err!("Missing v2 info"))?)
    }
}

#[derive(Debug)]
pub enum Device {
    V1(DFv1Device),
    V2(DFv2Node),
}

impl Device {
    pub fn get_device_info(&self) -> &fdd::NodeInfo {
        match self {
            Device::V1(device) => &device.0,
            Device::V2(node) => &node.0,
        }
    }

    /// Gets the full identifying path name of the device.
    ///
    /// For V1 devices, that is the `topological_path`.
    /// For V2 devices, that is the `moniker`.
    pub fn get_full_name(&self) -> Result<&str> {
        match self {
            Device::V1(device) => device.get_topo_path(),
            Device::V2(node) => node.get_moniker(),
        }
    }

    /// Gets the last ordinal of the identifying path name of the device.
    ///
    /// For V1 devices, it would be the part of the string after the last `/`.
    /// For V2 devices, it would be the part of the string after the last `.`.
    pub fn extract_name(&self) -> Result<&str> {
        match self {
            Device::V1(device) => device.extract_name(),
            Device::V2(node) => node.extract_name(),
        }
    }
}

impl std::convert::From<fdd::NodeInfo> for Device {
    fn from(device_info: fdd::NodeInfo) -> Device {
        match &device_info.versioned_info {
            Some(info) => match &info {
                fdd::VersionedNodeInfo::V1(_) => Device::V1(DFv1Device(device_info)),
                fdd::VersionedNodeInfo::V2(_) => Device::V2(DFv2Node(device_info)),
                _ => Device::V1(DFv1Device(device_info)),
            },
            None => Device::V1(DFv1Device(device_info)),
        }
    }
}

/// Combines pagination results into a single vector.
pub async fn get_device_info(
    service: &fdd::ManagerProxy,
    device_filter: &[String],
    exact_match: bool,
) -> Result<Vec<fdd::NodeInfo>> {
    let (iterator, iterator_server) =
        fidl::endpoints::create_proxy::<fdd::NodeInfoIteratorMarker>()?;

    service
        .get_node_info(device_filter, iterator_server, exact_match)
        .context("FIDL call to get device info failed")?;

    let mut info_result = Vec::new();
    loop {
        let mut device_info =
            iterator.get_next().await.context("FIDL call to get device info failed")?;
        if device_info.len() == 0 {
            break;
        }
        info_result.append(&mut device_info)
    }
    Ok(info_result)
}

/// Combines pagination results into a single vector.
pub async fn get_driver_info(
    service: &fdd::ManagerProxy,
    driver_filter: &[String],
) -> Result<Vec<fdf::DriverInfo>> {
    let (iterator, iterator_server) =
        fidl::endpoints::create_proxy::<fdd::DriverInfoIteratorMarker>()?;

    service
        .get_driver_info(driver_filter, iterator_server)
        .context("FIDL call to get driver info failed")?;

    let mut info_result = Vec::new();
    loop {
        let mut driver_info =
            iterator.get_next().await.context("FIDL call to get driver info failed")?;
        if driver_info.len() == 0 {
            break;
        }
        info_result.append(&mut driver_info)
    }
    Ok(info_result)
}

/// Combines pagination results into a single vector.
pub async fn get_composite_node_specs(
    service: &fdd::ManagerProxy,
    name_filter: Option<String>,
) -> Result<Vec<fdf::CompositeInfo>> {
    let (iterator, iterator_server) =
        fidl::endpoints::create_proxy::<fdd::CompositeNodeSpecIteratorMarker>()?;

    service
        .get_composite_node_specs(name_filter.as_deref(), iterator_server)
        .context("FIDL call to get node groups failed")?;

    let mut info_result = Vec::new();
    loop {
        let mut node_groups =
            iterator.get_next().await.context("FIDL call to get node groups failed")?;
        if node_groups.is_empty() {
            break;
        }
        info_result.append(&mut node_groups)
    }
    Ok(info_result)
}

/// Gets the desired DriverInfo instance.
///
/// Filter based on the driver's URL.
/// For example: "fuchsia-boot://domain/#meta/foo.cm"
///
/// # Arguments
/// * `driver_filter` - Filter to the driver that matches the given filter.
pub async fn get_driver_by_filter(
    driver_filter: &String,
    driver_development_proxy: &fdd::ManagerProxy,
) -> Result<fdf::DriverInfo> {
    let filter_list: [String; 1] = [driver_filter.to_string()];
    let driver_list = get_driver_info(&driver_development_proxy, &filter_list).await?;
    if driver_list.len() != 1 {
        return Err(anyhow!(
            "There should be exactly one match for '{}'. Found {}.",
            driver_filter,
            driver_list.len()
        ));
    }
    let mut driver_info: Option<fdf::DriverInfo> = None;

    // Confirm this is the correct match.
    let driver = &driver_list[0];
    if let Some(ref url) = driver.url {
        if url == driver_filter {
            driver_info = Some(driver.clone());
        }
    }
    match driver_info {
        Some(driver) => Ok(driver),
        _ => Err(anyhow!("Did not find matching driver for: {}", driver_filter)),
    }
}

/// Gets the driver that is bound to the given device.
///
/// Is able to fuzzy match on the device's topological path, where the shortest match
/// will be the one chosen.
///
/// # Arguments
/// * `device_topo_path` - The device's topological path. e.g. sys/platform/.../device
pub async fn get_driver_by_device(
    device_topo_path: &String,
    driver_development_proxy: &fdd::ManagerProxy,
) -> Result<fdf::DriverInfo> {
    let device_filter: [String; 1] = [device_topo_path.to_string()];
    let mut device_list =
        get_device_info(&driver_development_proxy, &device_filter, /* exact_match= */ true).await?;
    if device_list.len() != 1 {
        let fuzzy_device_list = get_device_info(
            &driver_development_proxy,
            &device_filter,
            /* exact_match= */ false,
        )
        .await?;
        if fuzzy_device_list.len() == 0 {
            return Err(anyhow!("No devices matched the query: {}", device_topo_path.to_string()));
        } else if fuzzy_device_list.len() > 1 {
            let mut builder = "Found multiple matches. Did you mean one of these?\n\n".to_string();
            for item in fuzzy_device_list {
                let device: Device = item.into();
                // We don't appear to have a string builder crate in-tree.
                builder = format!("{}{}\n", builder, device.get_full_name()?.to_string());
            }
            return Err(anyhow!(builder.to_string()));
        }
        device_list = fuzzy_device_list;
    }

    let found_device = device_list.remove(0);
    match found_device.bound_driver_url {
        Some(ref driver_filter) => {
            get_driver_by_filter(&driver_filter, &driver_development_proxy).await
        }
        _ => Err(anyhow!("Did not find driver for device {}", &device_topo_path)),
    }
}

/// Gets the devices that are bound to the given driver.
///
/// Filter based on the driver's URL.
/// For example: "fuchsia-boot://domain/#meta/foo.cm"
///
/// # Arguments
/// * `driver_filter` - Filter to the driver that matches the given filter.
pub async fn get_devices_by_driver(
    driver_filter: &String,
    driver_development_proxy: &fdd::ManagerProxy,
) -> Result<Vec<Device>> {
    let driver_info = get_driver_by_filter(driver_filter, &driver_development_proxy);
    let empty: [String; 0] = [];
    let device_list =
        get_device_info(&driver_development_proxy, &empty, /* exact_match= */ false);

    let (driver_info, device_list) = futures::join!(driver_info, device_list);
    let (driver_info, device_list) = (driver_info?, device_list?);

    let mut matches: Vec<Device> = Vec::new();
    for device_item in device_list.into_iter() {
        let device: Device = device_item.into();
        if let (Some(bound_driver_url), Some(url)) =
            (&device.get_device_info().bound_driver_url, &driver_info.url)
        {
            if &url == &bound_driver_url {
                matches.push(device);
            }
        }
    }
    Ok(matches)
}
