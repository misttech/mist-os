// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! This module contains code for converting between the sdk_metadata types and the engine
//! interface types. We perform the conversion here to keep dependencies on the sdk_metadata
//! to a minimum, while improving our ability to fully test the conversion code.

use anyhow::{anyhow, bail, Context, Result};
use assembly_manifest::Image;
use camino::Utf8PathBuf;
use emulator_instance::{
    DeviceConfig, DiskImage, EmulatorConfiguration, GuestConfig, PortMapping, VirtualCpu,
};

use sdk_metadata::{
    ProductBundle, ProductBundleV2, VirtualDevice, VirtualDeviceManifest, VirtualDeviceV1,
};
use std::path::PathBuf;

pub async fn convert_bundle_to_configs(
    product_bundle: &ProductBundle,
    device_name: Option<String>,
    uefi: bool,
) -> Result<EmulatorConfiguration> {
    match &product_bundle {
        ProductBundle::V2(product_bundle) => {
            let virtual_device = if let Some(device) = parse_device_name_as_path(&device_name) {
                device
            } else {
                // Determine the correct device name from the user, or default to the "recommended"
                // device, if one is provided in the product bundle.
                let path = product_bundle.get_virtual_devices_path();
                let manifest =
                    VirtualDeviceManifest::from_path(&path).context("manifest from_path")?;
                let result = match device_name.as_deref() {
                    // If no device_name is given, return the default specified in the manifest.
                    None | Some("") => manifest.default_device(),

                    // Otherwise, find the virtual device by name in the product bundle.
                    Some(device_name) => manifest.device(device_name).map(|d| Some(d)),
                }?;
                match result {
                    Some(virtual_device) => virtual_device,
                    None if device_name.is_some() => bail!(
                        "No virtual device matches '{}'.",
                        device_name.unwrap_or_else(|| "<empty>".to_string())
                    ),
                    None => {
                        bail!("No default virtual device is available, please specify one by name.")
                    }
                }
            };
            let virtual_device = match virtual_device {
                VirtualDevice::V1(v) => v,
            };
            tracing::debug!(
                "Found PBM: {:#?}\nVirtual Device: {:#?}",
                &product_bundle,
                &virtual_device
            );
            convert_v2_bundle_to_configs(&product_bundle, &virtual_device, uefi)
        }
    }
}

/// If the user passes in a --device flag with a path to a virtual device file
/// instead of a device name, we want to use the custom device. If it's not a
/// file, that's ok, we'll still try it as a device name; so this function
/// doesn't return an Error, just None.
fn parse_device_name_as_path(path: &Option<String>) -> Option<VirtualDevice> {
    let cwd = std::env::current_dir().ok()?;
    path.as_ref().and_then(|name| {
        if name.is_empty() {
            return None;
        }
        // See if the "name" is actually a path to a virtual device file.
        let path =
            Utf8PathBuf::from_path_buf(cwd).expect("Current directory is not utf8").join(name);
        if !path.is_file() {
            tracing::debug!("Value '{}' doesn't appear to be a valid file.", name);
            return None;
        }
        match VirtualDevice::try_load_from(&path) {
            Ok(VirtualDevice::V1(vd)) => Some(VirtualDevice::V1(vd)),
            Err(_) => {
                eprintln!(
                    "Attempted to use the file at '{}' to configure the device, but the contents \
                    of that file are not a valid Virtual Device specification. \nChecking the \
                    Product Bundle for a device with that name...",
                    path
                );
                tracing::warn!("Path '{}' doesn't contain a valid virtual device.", path);
                None
            }
        }
    })
}

fn convert_v2_bundle_to_configs(
    product_bundle: &ProductBundleV2,
    virtual_device: &VirtualDeviceV1,
    uefi: bool,
) -> Result<EmulatorConfiguration> {
    let mut emulator_configuration: EmulatorConfiguration = EmulatorConfiguration::default();

    // Map the product and device specifications to the Device, and Guest configs.
    emulator_configuration.device = DeviceConfig {
        audio: virtual_device.hardware.audio.clone(),
        cpu: VirtualCpu {
            architecture: virtual_device.hardware.cpu.arch.clone(),
            count: virtual_device.hardware.cpu.count,
        },
        memory: virtual_device.hardware.memory.clone(),
        pointing_device: virtual_device.hardware.inputs.pointing_device.clone(),
        screen: virtual_device.hardware.window_size.clone(),
        storage: virtual_device.hardware.storage.clone(),
        vsock: Some(virtual_device.hardware.vsock.clone()),
    };

    emulator_configuration.runtime.template = None;

    if let Some(ports) = &virtual_device.ports {
        for (name, port) in ports {
            emulator_configuration
                .host
                .port_map
                .insert(name.to_owned(), PortMapping { guest: port.to_owned(), host: None });
        }
    }

    // Try to find images in system_a.
    let system = product_bundle
        .system_a
        .as_ref()
        .ok_or_else(|| anyhow!("No systems to boot in the product bundle"))?;

    let kernel_image: Option<PathBuf> = system.iter().find_map(|i| match i {
        Image::QemuKernel(path) => Some(path.clone().into()),
        _ => None,
    });

    let mut disk_image: Option<DiskImage> = system.iter().find_map(|i| match (uefi, i) {
        (false, Image::FVM(path)) => Some(DiskImage::Fvm(path.clone().into())),
        (false, Image::Fxfs { path, .. }) => Some(DiskImage::Fxfs(path.clone().into())),
        (true, Image::Fxfs { path, .. }) => Some(DiskImage::Gpt(path.clone().into())),
        _ => None,
    });

    // If there is no kernel, and no disk, check for a bootloader file.
    if disk_image.is_none() && kernel_image.is_none() {
        if let Some(bootloader) = product_bundle.partitions.bootloader_partitions.iter().next() {
            disk_image = Some(DiskImage::Fat(bootloader.image.clone().into()));
        } else {
            bail!("No kernel or bootloader specified in the configuration.")
        }
    }

    // Some kernels do not have separate zbi images.
    let zbi_image: Option<PathBuf> = system.iter().find_map(|i| match i {
        Image::ZBI { path, .. } => Some(path.clone().into()),
        _ => None,
    });

    emulator_configuration.guest =
        GuestConfig { disk_image, kernel_image, zbi_image, ..Default::default() };

    Ok(emulator_configuration)
}

#[cfg(test)]
mod tests {
    use super::*;
    use assembly_manifest::BlobfsContents;
    use assembly_partitions_config::{BootloaderPartition, PartitionsConfig};
    use sdk_metadata::virtual_device::{Cpu, Hardware};
    use sdk_metadata::{
        AudioDevice, AudioModel, CpuArchitecture, DataAmount, DataUnits, ElementType, InputDevice,
        PointingDevice, Screen, ScreenUnits, VsockDevice,
    };
    use std::collections::HashMap;
    use std::fs::File;
    use std::io::Write;

    const VIRTUAL_DEVICE_VALID: &str =
        include_str!("../../../../../../../build/sdk/meta/test_data/virtual_device.json");

    #[test]
    fn test_convert_v2_bundle_to_configs() {
        let temp_dir = tempfile::TempDir::new().expect("creating sdk_root temp dir");
        let sdk_root = temp_dir.path();
        let template_path = sdk_root.join("fake_template");
        std::fs::write(&template_path, b"").expect("create fake template file");

        // Set up some test data to pass into the conversion routine.
        let expected_kernel = Utf8PathBuf::from_path_buf(sdk_root.join("kernel"))
            .expect("couldn't convert kernel to utf8");
        let expected_disk_image_path =
            Utf8PathBuf::from_path_buf(sdk_root.join("fvm")).expect("couldn't convert fvm to utf8");
        let expected_zbi =
            Utf8PathBuf::from_path_buf(sdk_root.join("zbi")).expect("couldn't convert zbi to utf8");

        let mut pb = ProductBundleV2 {
            product_name: String::default(),
            product_version: String::default(),
            partitions: PartitionsConfig::default(),
            sdk_version: String::default(),
            system_a: Some(vec![
                // By the time we call convert_, these should be canonicalized.
                Image::ZBI { path: expected_zbi.clone(), signed: false },
                Image::QemuKernel(expected_kernel.clone()),
                Image::FVM(expected_disk_image_path.clone()),
            ]),
            system_b: None,
            system_r: None,
            repositories: vec![],
            update_package_hash: None,
            virtual_devices_path: None,
        };
        let mut device = VirtualDeviceV1 {
            name: "FakeDevice".to_string(),
            description: Some("A fake virtual device".to_string()),
            kind: ElementType::VirtualDevice,
            hardware: Hardware {
                cpu: Cpu { arch: CpuArchitecture::X64, count: 4 },
                audio: AudioDevice { model: AudioModel::Hda },
                storage: DataAmount { quantity: 512, units: DataUnits::Megabytes },
                inputs: InputDevice { pointing_device: PointingDevice::Mouse },
                memory: DataAmount { quantity: 4, units: DataUnits::Gigabytes },
                window_size: Screen { height: 480, width: 640, units: ScreenUnits::Pixels },
                vsock: VsockDevice { enabled: false, cid: 3 },
            },
            ports: None,
        };

        // Run the conversion, then assert everything in the config matches the manifest data.
        let config = convert_v2_bundle_to_configs(&pb, &device, false)
            .expect("convert_v2_bundle_to_configs");
        assert_eq!(config.device.audio, device.hardware.audio);
        assert_eq!(config.device.cpu.architecture, device.hardware.cpu.arch);
        assert_eq!(config.device.memory, device.hardware.memory);
        assert_eq!(config.device.pointing_device, device.hardware.inputs.pointing_device);
        assert_eq!(config.device.screen, device.hardware.window_size);
        assert_eq!(config.device.storage, device.hardware.storage);
        assert_eq!(config.device.vsock, Some(device.hardware.vsock));

        assert!(config.guest.disk_image.is_some());

        assert_eq!(
            config.guest.disk_image.unwrap(),
            DiskImage::Fvm(expected_disk_image_path.into())
        );
        assert_eq!(config.guest.kernel_image, Some(expected_kernel.into()));
        assert_eq!(config.guest.zbi_image, Some(expected_zbi.into_std_path_buf()));

        assert_eq!(config.host.port_map.len(), 0);

        // Adjust all of the values that affect the config, then run it again.
        let expected_kernel = Utf8PathBuf::from_path_buf(sdk_root.join("some/new_kernel"))
            .expect("couldn't convert kernel to utf8");
        let expected_disk_image_path = Utf8PathBuf::from_path_buf(sdk_root.join("fxfs"))
            .expect("couldn't convert fxfs to utf8");
        let expected_zbi = Utf8PathBuf::from_path_buf(sdk_root.join("path/to/new_zbi"))
            .expect("couldn't convert zbi to utf8");

        pb.system_a = Some(vec![
            Image::ZBI { path: expected_zbi.clone(), signed: false },
            Image::QemuKernel(expected_kernel.clone()),
            Image::Fxfs {
                path: expected_disk_image_path.clone(),
                contents: BlobfsContents::default(),
            },
        ]);
        device.hardware = Hardware {
            cpu: Cpu { arch: CpuArchitecture::Arm64, count: 4 },
            audio: AudioDevice { model: AudioModel::None },
            storage: DataAmount { quantity: 8, units: DataUnits::Gigabytes },
            inputs: InputDevice { pointing_device: PointingDevice::Touch },
            memory: DataAmount { quantity: 2048, units: DataUnits::Megabytes },
            window_size: Screen { height: 1024, width: 1280, units: ScreenUnits::Pixels },
            vsock: VsockDevice { enabled: false, cid: 3 },
        };

        let mut ports = HashMap::new();
        ports.insert("ssh".to_string(), 22);
        ports.insert("debug".to_string(), 2345);
        device.ports = Some(ports);

        let mut config = convert_v2_bundle_to_configs(&pb, &device, false)
            .expect("convert_bundle_v2_to_configs");

        // Verify that all of the new values are loaded and match the new manifest data.
        assert_eq!(config.device.audio, device.hardware.audio);
        assert_eq!(config.device.cpu.architecture, device.hardware.cpu.arch);
        assert_eq!(config.device.memory, device.hardware.memory);
        assert_eq!(config.device.pointing_device, device.hardware.inputs.pointing_device);
        assert_eq!(config.device.screen, device.hardware.window_size);
        assert_eq!(config.device.storage, device.hardware.storage);
        assert_eq!(config.device.vsock, Some(device.hardware.vsock));

        assert!(config.guest.disk_image.is_some());

        assert_eq!(
            config.guest.disk_image.unwrap(),
            DiskImage::Fxfs(expected_disk_image_path.into())
        );
        assert_eq!(config.guest.kernel_image, Some(expected_kernel.into()));
        assert_eq!(config.guest.zbi_image, Some(expected_zbi.into_std_path_buf()));

        assert_eq!(config.host.port_map.len(), 2);
        assert!(config.host.port_map.contains_key("ssh"));
        assert_eq!(
            config.host.port_map.remove("ssh").unwrap(),
            PortMapping { host: None, guest: 22 }
        );
        assert!(config.host.port_map.contains_key("debug"));
        assert_eq!(
            config.host.port_map.remove("debug").unwrap(),
            PortMapping { host: None, guest: 2345 }
        );
    }

    #[test]
    fn test_convert_v2_bundle_to_uefi_config() {
        let temp_dir = tempfile::TempDir::new().expect("creating sdk_root temp dir");
        let sdk_root = temp_dir.path();
        let template_path = sdk_root.join("fake_template");
        std::fs::write(&template_path, b"").expect("create fake template file");

        let expected_kernel = Utf8PathBuf::from_path_buf(sdk_root.join("some/new_kernel"))
            .expect("couldn't convert kernel to utf8");
        let expected_disk_image_path = Utf8PathBuf::from_path_buf(sdk_root.join("fxfs"))
            .expect("couldn't convert fxfs to utf8");
        let expected_zbi = Utf8PathBuf::from_path_buf(sdk_root.join("path/to/new_zbi"))
            .expect("couldn't convert zbi to utf8");

        let pb = ProductBundleV2 {
            product_name: String::default(),
            product_version: String::default(),
            partitions: PartitionsConfig::default(),
            sdk_version: String::default(),
            system_a: Some(vec![
                Image::ZBI { path: expected_zbi.clone(), signed: false },
                Image::QemuKernel(expected_kernel.clone()),
                Image::Fxfs {
                    path: expected_disk_image_path.clone(),
                    contents: BlobfsContents::default(),
                },
            ]),
            system_b: None,
            system_r: None,
            repositories: vec![],
            update_package_hash: None,
            virtual_devices_path: None,
        };
        let mut device = VirtualDeviceV1 {
            name: "FakeDevice".to_string(),
            description: Some("A fake virtual device".to_string()),
            kind: ElementType::VirtualDevice,
            hardware: Hardware {
                cpu: Cpu { arch: CpuArchitecture::X64, count: 4 },
                audio: AudioDevice { model: AudioModel::Hda },
                storage: DataAmount { quantity: 512, units: DataUnits::Megabytes },
                inputs: InputDevice { pointing_device: PointingDevice::Mouse },
                memory: DataAmount { quantity: 4, units: DataUnits::Gigabytes },
                window_size: Screen { height: 480, width: 640, units: ScreenUnits::Pixels },
                vsock: VsockDevice { enabled: false, cid: 3 },
            },
            ports: None,
        };
        let mut ports = HashMap::new();
        ports.insert("ssh".to_string(), 22);
        ports.insert("debug".to_string(), 2345);
        device.ports = Some(ports);

        let mut config =
            convert_v2_bundle_to_configs(&pb, &device, true).expect("convert_bundle_v2_to_configs");

        assert_eq!(config.device.audio, device.hardware.audio);
        assert_eq!(config.device.cpu.architecture, device.hardware.cpu.arch);
        assert_eq!(config.device.memory, device.hardware.memory);
        assert_eq!(config.device.pointing_device, device.hardware.inputs.pointing_device);
        assert_eq!(config.device.screen, device.hardware.window_size);
        assert_eq!(config.device.storage, device.hardware.storage);
        assert_eq!(config.device.vsock, Some(device.hardware.vsock));

        assert!(config.guest.disk_image.is_some());

        // After converting the v2 bundle to config, the resulting guest config should contain the
        // newly assembled GPT full disk image, not the source Fxfs image.
        assert_eq!(
            config.guest.disk_image.unwrap(),
            DiskImage::Gpt(expected_disk_image_path.into())
        );
        assert_eq!(config.guest.kernel_image, Some(expected_kernel.into()));
        assert_eq!(config.guest.zbi_image, Some(expected_zbi.into_std_path_buf()));

        assert_eq!(config.host.port_map.len(), 2);
        assert!(config.host.port_map.contains_key("ssh"));
        assert_eq!(
            config.host.port_map.remove("ssh").unwrap(),
            PortMapping { host: None, guest: 22 }
        );
        assert!(config.host.port_map.contains_key("debug"));
        assert_eq!(
            config.host.port_map.remove("debug").unwrap(),
            PortMapping { host: None, guest: 2345 }
        );
    }

    #[test]
    fn test_parse_device_name_as_path_none() {
        assert_eq!(parse_device_name_as_path(&None), None);
    }

    #[test]
    fn test_parse_device_name_as_path_empty() {
        assert_eq!(parse_device_name_as_path(&Some(String::new())), None);
    }

    #[test]
    fn test_parse_device_name_as_path_no_file() {
        assert_eq!(parse_device_name_as_path(&Some("SomeNameThatsNotAFile".to_string())), None);
    }

    #[test]
    fn test_parse_device_name_as_path_other_file() -> Result<()> {
        let temp_dir = tempfile::TempDir::new().expect("creating temp dir");
        let path = temp_dir.path().join("other_file.json");
        File::create(&path)?;
        assert_eq!(parse_device_name_as_path(&Some(path.to_string_lossy().into_owned())), None);
        Ok(())
    }

    #[test]
    fn test_parse_device_name_as_path_ok() -> Result<()> {
        let temp_dir = tempfile::TempDir::new().expect("creating temp dir");
        let path = temp_dir.path().join("device.json");
        let mut file = File::create(&path).unwrap();
        file.write_all(VIRTUAL_DEVICE_VALID.as_bytes())?;
        let result = parse_device_name_as_path(&Some(path.to_string_lossy().into_owned()));
        assert!(matches!(result, Some(VirtualDevice::V1(_))));
        Ok(())
    }

    #[test]
    fn test_efi_product_bundle() {
        let temp_dir = tempfile::TempDir::new().expect("creating sdk_root temp dir");
        let sdk_root = temp_dir.path();

        // Set up some test data to pass into the conversion routine.
        let expected_kernel = Utf8PathBuf::from_path_buf(sdk_root.join("kernel.efi"))
            .expect("couldn't convert kernel to utf8");

        let pb = ProductBundleV2 {
            product_name: String::default(),
            product_version: String::default(),
            partitions: PartitionsConfig::default(),
            sdk_version: String::default(),
            system_a: Some(vec![Image::QemuKernel(expected_kernel.clone())]),
            system_b: None,
            system_r: None,
            repositories: vec![],
            update_package_hash: None,
            virtual_devices_path: None,
        };
        let device = VirtualDeviceV1 {
            name: "FakeDevice".to_string(),
            description: Some("A fake virtual device".to_string()),
            kind: ElementType::VirtualDevice,
            hardware: Hardware {
                cpu: Cpu { arch: CpuArchitecture::X64, count: 4 },
                audio: AudioDevice { model: AudioModel::Hda },
                storage: DataAmount { quantity: 512, units: DataUnits::Megabytes },
                inputs: InputDevice { pointing_device: PointingDevice::Mouse },
                memory: DataAmount { quantity: 4, units: DataUnits::Gigabytes },
                window_size: Screen { height: 480, width: 640, units: ScreenUnits::Pixels },
                vsock: VsockDevice { enabled: false, cid: 3 },
            },
            ports: None,
        };

        // Run the conversion, then assert everything in the config matches the manifest data.
        let config = convert_v2_bundle_to_configs(&pb, &device, false)
            .expect("convert_v2_bundle_to_configs");
        assert_eq!(config.device.audio, device.hardware.audio);
        assert_eq!(config.device.cpu.architecture, device.hardware.cpu.arch);
        assert_eq!(config.device.memory, device.hardware.memory);
        assert_eq!(config.device.pointing_device, device.hardware.inputs.pointing_device);
        assert_eq!(config.device.screen, device.hardware.window_size);
        assert_eq!(config.device.storage, device.hardware.storage);
        assert_eq!(config.device.vsock, Some(device.hardware.vsock));

        assert!(config.guest.disk_image.is_none());
        assert!(config.guest.zbi_image.is_none());

        assert_eq!(config.guest.kernel_image, Some(expected_kernel.into()));

        assert_eq!(config.host.port_map.len(), 0);
    }

    #[test]
    fn test_bootloader_fatfs_product_bundle() {
        let pb = ProductBundleV2 {
            product_name: String::default(),
            product_version: String::default(),
            partitions: PartitionsConfig {
                bootloader_partitions: vec![BootloaderPartition {
                    partition_type: "fat".into(),
                    name: Some("some-efi-shell.fatfs".into()),
                    image: "partitions/bootloaders/some-efi-shell.fat".into(),
                }],
                hardware_revision: "x64".into(),
                ..Default::default()
            },

            sdk_version: String::default(),
            system_a: Some(vec![]),
            system_b: None,
            system_r: None,
            repositories: vec![],
            update_package_hash: None,
            virtual_devices_path: None,
        };
        let device = VirtualDeviceV1 {
            name: "FakeDevice".to_string(),
            description: Some("A fake virtual device".to_string()),
            kind: ElementType::VirtualDevice,
            hardware: Hardware {
                cpu: Cpu { arch: CpuArchitecture::X64, count: 4 },
                audio: AudioDevice { model: AudioModel::Hda },
                storage: DataAmount { quantity: 512, units: DataUnits::Megabytes },
                inputs: InputDevice { pointing_device: PointingDevice::Mouse },
                memory: DataAmount { quantity: 4, units: DataUnits::Gigabytes },
                window_size: Screen { height: 480, width: 640, units: ScreenUnits::Pixels },
                vsock: VsockDevice { enabled: false, cid: 3 },
            },
            ports: None,
        };

        // Run the conversion, then assert everything in the config matches the manifest data.
        let config = convert_v2_bundle_to_configs(&pb, &device, false)
            .expect("convert_v2_bundle_to_configs");
        assert_eq!(config.device.audio, device.hardware.audio);
        assert_eq!(config.device.cpu.architecture, device.hardware.cpu.arch);
        assert_eq!(config.device.memory, device.hardware.memory);
        assert_eq!(config.device.pointing_device, device.hardware.inputs.pointing_device);
        assert_eq!(config.device.screen, device.hardware.window_size);
        assert_eq!(config.device.storage, device.hardware.storage);
        assert_eq!(config.device.vsock, Some(device.hardware.vsock));

        assert_eq!(
            config.guest.disk_image,
            Some(DiskImage::Fat("partitions/bootloaders/some-efi-shell.fat".into()))
        );
        assert!(config.guest.zbi_image.is_none());

        assert!(config.guest.kernel_image.is_none());

        assert_eq!(config.host.port_map.len(), 0);
    }
}
