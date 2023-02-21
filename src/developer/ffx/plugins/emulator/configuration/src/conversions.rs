// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! This module contains code for converting between the sdk_metadata types and the engine
//! interface types. We perform the conversion here to keep dependencies on the sdk_metadata
//! to a minimum, while improving our ability to fully test the conversion code.

use crate::{DeviceConfig, EmulatorConfiguration, GuestConfig, PortMapping, VirtualCpu};
use anyhow::{anyhow, bail, Context, Result};
use assembly_manifest::Image;
use camino::Utf8PathBuf;
use pbms::{
    fms_entries_from, get_images_dir, load_product_bundle, select_product_bundle, ListingMode,
};
use sdk_metadata::{
    ProductBundle, ProductBundleV1, ProductBundleV2, VirtualDevice, VirtualDeviceManifest,
    VirtualDeviceV1,
};
use std::path::{Path, PathBuf};

pub async fn convert_bundle_to_configs(
    product_bundle_name: Option<String>,
    device_name: Option<String>,
    verbose: bool,
) -> Result<EmulatorConfiguration> {
    let sdk = ffx_config::global_env_context()
        .context("loading global environment context")?
        .get_sdk()
        .await?;
    let product_bundle =
        load_product_bundle(&sdk, &product_bundle_name, ListingMode::ReadyBundlesOnly).await?;
    match &product_bundle {
        ProductBundle::V1(product_bundle) => {
            let should_print = false;
            let product_url = select_product_bundle(
                &sdk,
                &Some(product_bundle.name.clone()),
                ListingMode::ReadyBundlesOnly,
                should_print,
            )
            .await
            .context("Selecting product bundle")?;

            // Find the data root, which is used to find the images and template file.
            let data_root =
                get_images_dir(&product_url, sdk.get_path_prefix()).await.context("images dir")?;

            let virtual_device = if let Some(device) = parse_device_name_as_path(&device_name) {
                device
            } else {
                // Get the virtual device from the bundle.
                let fms_entries = fms_entries_from(&product_url, sdk.get_path_prefix())
                    .await
                    .context("get fms entries")?;
                let virtual_devices =
                    fms::find_virtual_devices(&fms_entries, &product_bundle.device_refs)
                        .context("problem with virtual device")?;

                // Determine the correct device name from the user, or default to the first one
                // listed in the product bundle.
                let device = match device_name.as_deref() {
                    // If no device_name is given, choose the first virtual device listed in
                    // the product bundle.
                    None | Some("") => virtual_devices.get(0).ok_or_else(|| {
                        anyhow!("There are no virtual devices in this product bundle.")
                    })?,

                    // Otherwise, find the virtual device by name in the product bundle.
                    Some(device_name) => virtual_devices
                        .iter()
                        .find(|vd| vd.name() == device_name)
                        .ok_or_else(|| {
                            anyhow!(
                                "The device '{}' is not found in the product bundle.",
                                device_name
                            )
                        })?,
                };
                device.clone()
            };
            let virtual_device = match virtual_device {
                VirtualDevice::V1(v) => v,
            };
            if verbose {
                println!(
                    "Found PBM: {:?}, device_refs: {:?}, virtual_device: {:#?}",
                    &product_bundle.name, &product_bundle.device_refs, &virtual_device
                );
            }
            convert_v1_bundle_to_configs(product_bundle, &virtual_device, &data_root)
                .context("problem with internal conversion")
        }
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
                        device_name.unwrap_or("<empty>".to_string())
                    ),
                    None => {
                        bail!("No default virtual device is available, please specify one by name.")
                    }
                }
            };
            let virtual_device = match virtual_device {
                VirtualDevice::V1(v) => v,
            };
            if verbose {
                println!(
                    "Found PBM: {:#?}\nVirtual Device: {:#?}",
                    &product_bundle, &virtual_device
                );
            }
            convert_v2_bundle_to_configs(&product_bundle, &virtual_device)
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
            Ok(VirtualDevice::V1(mut vd)) => {
                // The template file path is relative to the device file.
                tracing::debug!("Using file '{}' as a virtual device.", path);
                let template = vd.start_up_args_template;
                // The path was successfully used for a device file, so it must
                // have a parent, and this '.unwrap()' will never fail.
                let parent = path.parent().unwrap();
                vd.start_up_args_template = parent.join(template);
                Some(VirtualDevice::V1(vd))
            }
            Err(_) => {
                println!(
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

/// - `data_root` is a path to a directory. When working in-tree it's the path
///   to build output dir; when using the SDK it's the path to the downloaded
///   images directory.
fn convert_v1_bundle_to_configs(
    product_bundle: &ProductBundleV1,
    virtual_device: &VirtualDeviceV1,
    data_root: &Path,
) -> Result<EmulatorConfiguration> {
    let mut emulator_configuration: EmulatorConfiguration = EmulatorConfiguration::default();

    // Map the product and device specifications to the Device, and Guest configs.
    emulator_configuration.device = DeviceConfig {
        audio: virtual_device.hardware.audio.clone(),
        cpu: VirtualCpu {
            architecture: virtual_device.hardware.cpu.arch.clone(),
            // TODO(fxbug.dev/88909): Add a count parameter to the virtual_device cpu field.
            count: usize::default(),
        },
        memory: virtual_device.hardware.memory.clone(),
        pointing_device: virtual_device.hardware.inputs.pointing_device.clone(),
        screen: virtual_device.hardware.window_size.clone(),
        storage: virtual_device.hardware.storage.clone(),
    };

    let template = &virtual_device.start_up_args_template;
    emulator_configuration.runtime.template = data_root
        .join(&template)
        .canonicalize()
        .with_context(|| format!("canonicalize template path {:?}", data_root.join(&template)))?;

    if let Some(ports) = &virtual_device.ports {
        for (name, port) in ports {
            emulator_configuration
                .host
                .port_map
                .insert(name.to_owned(), PortMapping { guest: port.to_owned(), host: None });
        }
    }

    if let Some(emu) = &product_bundle.manifests.emu {
        emulator_configuration.guest = GuestConfig {
            // TODO(fxbug.dev/88908): Eventually we'll need to support multiple disk_images.
            fvm_image: if !emu.disk_images.is_empty() {
                Some(data_root.join(&emu.disk_images[0]))
            } else {
                None
            },
            kernel_image: data_root.join(&emu.kernel),
            zbi_image: data_root.join(&emu.initial_ramdisk),
        };
    } else {
        return Err(anyhow!(
            "The Product Bundle specified by {} does not contain any Emulator Manifests.",
            &product_bundle.name
        ));
    }
    Ok(emulator_configuration)
}

fn convert_v2_bundle_to_configs(
    product_bundle: &ProductBundleV2,
    virtual_device: &VirtualDeviceV1,
) -> Result<EmulatorConfiguration> {
    let mut emulator_configuration: EmulatorConfiguration = EmulatorConfiguration::default();

    // Map the product and device specifications to the Device, and Guest configs.
    emulator_configuration.device = DeviceConfig {
        audio: virtual_device.hardware.audio.clone(),
        cpu: VirtualCpu {
            architecture: virtual_device.hardware.cpu.arch.clone(),
            // TODO(fxbug.dev/88909): Add a count parameter to the virtual_device cpu field.
            count: usize::default(),
        },
        memory: virtual_device.hardware.memory.clone(),
        pointing_device: virtual_device.hardware.inputs.pointing_device.clone(),
        screen: virtual_device.hardware.window_size.clone(),
        storage: virtual_device.hardware.storage.clone(),
    };

    let template = &virtual_device.start_up_args_template;
    emulator_configuration.runtime.template = PathBuf::from(template)
        .canonicalize()
        .with_context(|| format!("canonicalize template path {:?}", &template))?;

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
        .ok_or(anyhow!("No systems to boot in the product bundle"))?;

    let kernel_image = system
        .iter()
        .find_map(|i| match i {
            Image::QemuKernel(path) => Some(path.clone().into()),
            _ => None,
        })
        .ok_or(anyhow!("No emulator kernels specified in the product bundle"))?;

    let fvm_image: Option<PathBuf> = system.iter().find_map(|i| match i {
        Image::FVM(path) => Some(path.clone().into()),
        _ => None,
    });

    let zbi_image: PathBuf = system
        .iter()
        .find_map(|i| match i {
            Image::ZBI { path, .. } => Some(path.clone().into()),
            _ => None,
        })
        .ok_or(anyhow!("No ZBI in the product bundle"))?;

    emulator_configuration.guest = GuestConfig { fvm_image, kernel_image, zbi_image };

    Ok(emulator_configuration)
}

#[cfg(test)]
mod tests {
    use super::*;
    use assembly_partitions_config::PartitionsConfig;
    use sdk_metadata::{
        virtual_device::{Cpu, Hardware},
        AudioDevice, AudioModel, CpuArchitecture, DataAmount, DataUnits, ElementType, EmuManifest,
        InputDevice, Manifests, PointingDevice, Screen, ScreenUnits,
    };
    use sdk_metadata::{ProductBundleV1, VirtualDeviceV1};
    use std::{collections::HashMap, fs::File, io::Write};

    const VIRTUAL_DEVICE_VALID: &str =
        include_str!("../../../../../../../build/sdk/meta/test_data/virtual_device.json");

    #[test]
    fn test_convert_v1_bundle_to_configs() {
        let temp_dir = tempfile::TempDir::new().expect("creating sdk_root temp dir");
        let sdk_root = temp_dir.path();
        let template_path = sdk_root.join("fake_template");
        std::fs::write(&template_path, b"").expect("create fake template file");

        // Set up some test data to pass into the conversion routine.
        let mut pb = ProductBundleV1 {
            description: Some("A fake product bundle".to_string()),
            device_refs: vec!["".to_string()],
            images: vec![],
            manifests: Manifests {
                emu: Some(EmuManifest {
                    disk_images: vec!["path/to/disk".to_string()],
                    initial_ramdisk: "path/to/zbi".to_string(),
                    kernel: "path/to/kernel".to_string(),
                }),
                flash: None,
            },
            metadata: None,
            packages: vec![],
            name: "FakeBundle".to_string(),
            kind: ElementType::ProductBundle,
        };
        let mut device = VirtualDeviceV1 {
            name: "FakeDevice".to_string(),
            description: Some("A fake virtual device".to_string()),
            kind: ElementType::VirtualDevice,
            hardware: Hardware {
                cpu: Cpu { arch: CpuArchitecture::X64 },
                audio: AudioDevice { model: AudioModel::Hda },
                storage: DataAmount { quantity: 512, units: DataUnits::Megabytes },
                inputs: InputDevice { pointing_device: PointingDevice::Mouse },
                memory: DataAmount { quantity: 4, units: DataUnits::Gigabytes },
                window_size: Screen { height: 480, width: 640, units: ScreenUnits::Pixels },
            },
            start_up_args_template: Utf8PathBuf::from_path_buf(template_path.clone()).unwrap(),
            ports: None,
        };

        // Run the conversion, then assert everything in the config matches the manifest data.
        let config = convert_v1_bundle_to_configs(&pb, &device, &sdk_root)
            .expect("convert_bundle_to_configs");
        assert_eq!(config.device.audio, device.hardware.audio);
        assert_eq!(config.device.cpu.architecture, device.hardware.cpu.arch);
        assert_eq!(config.device.memory, device.hardware.memory);
        assert_eq!(config.device.pointing_device, device.hardware.inputs.pointing_device);
        assert_eq!(config.device.screen, device.hardware.window_size);
        assert_eq!(config.device.storage, device.hardware.storage);

        assert!(config.guest.fvm_image.is_some());
        let emu = pb.manifests.emu.unwrap();

        let expected_kernel = sdk_root.join(emu.kernel);
        let expected_fvm = sdk_root.join(&emu.disk_images[0]);
        let expected_zbi = sdk_root.join(emu.initial_ramdisk);

        assert_eq!(config.guest.fvm_image.unwrap(), expected_fvm);
        assert_eq!(config.guest.kernel_image, expected_kernel);
        assert_eq!(config.guest.zbi_image, expected_zbi);

        assert_eq!(config.host.port_map.len(), 0);

        // Adjust all of the values that affect the config, then run it again.
        pb.manifests = Manifests {
            emu: Some(EmuManifest {
                disk_images: vec!["different_path/to/disk".to_string()],
                initial_ramdisk: "different_path/to/zbi".to_string(),
                kernel: "different_path/to/kernel".to_string(),
            }),
            flash: None,
        };
        device.hardware = Hardware {
            cpu: Cpu { arch: CpuArchitecture::Arm64 },
            audio: AudioDevice { model: AudioModel::None },
            storage: DataAmount { quantity: 8, units: DataUnits::Gigabytes },
            inputs: InputDevice { pointing_device: PointingDevice::Touch },
            memory: DataAmount { quantity: 2048, units: DataUnits::Megabytes },
            window_size: Screen { height: 1024, width: 1280, units: ScreenUnits::Pixels },
        };
        device.start_up_args_template = Utf8PathBuf::from_path_buf(template_path).unwrap();

        let mut ports = HashMap::new();
        ports.insert("ssh".to_string(), 22);
        ports.insert("debug".to_string(), 2345);
        device.ports = Some(ports);

        let mut config = convert_v1_bundle_to_configs(&pb, &device, &sdk_root)
            .expect("convert_bundle_to_configs");

        // Verify that all of the new values are loaded and match the new manifest data.
        assert_eq!(config.device.audio, device.hardware.audio);
        assert_eq!(config.device.cpu.architecture, device.hardware.cpu.arch);
        assert_eq!(config.device.memory, device.hardware.memory);
        assert_eq!(config.device.pointing_device, device.hardware.inputs.pointing_device);
        assert_eq!(config.device.screen, device.hardware.window_size);
        assert_eq!(config.device.storage, device.hardware.storage);

        assert!(config.guest.fvm_image.is_some());
        let emu = pb.manifests.emu.unwrap();
        let expected_kernel = sdk_root.join(emu.kernel);
        let expected_fvm = sdk_root.join(&emu.disk_images[0]);
        let expected_zbi = sdk_root.join(emu.initial_ramdisk);

        assert_eq!(config.guest.fvm_image.unwrap(), expected_fvm);
        assert_eq!(config.guest.kernel_image, expected_kernel);
        assert_eq!(config.guest.zbi_image, expected_zbi);

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
    fn test_convert_v2_bundle_to_configs() {
        let temp_dir = tempfile::TempDir::new().expect("creating sdk_root temp dir");
        let sdk_root = temp_dir.path();
        let template_path = sdk_root.join("fake_template");
        std::fs::write(&template_path, b"").expect("create fake template file");

        // Set up some test data to pass into the conversion routine.
        let expected_kernel = Utf8PathBuf::from_path_buf(sdk_root.join("kernel"))
            .expect("couldn't convert kernel to utf8");
        let expected_fvm =
            Utf8PathBuf::from_path_buf(sdk_root.join("fvm")).expect("couldn't convert fvm to utf8");
        let expected_zbi =
            Utf8PathBuf::from_path_buf(sdk_root.join("zbi")).expect("couldn't convert zbi to utf8");

        let mut pb = ProductBundleV2 {
            product_name: String::default(),
            partitions: PartitionsConfig::default(),
            system_a: Some(vec![
                // By the time we call convert_, these should be canonicalized.
                Image::ZBI { path: expected_zbi.clone(), signed: false },
                Image::QemuKernel(expected_kernel.clone()),
                Image::FVM(expected_fvm.clone()),
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
                cpu: Cpu { arch: CpuArchitecture::X64 },
                audio: AudioDevice { model: AudioModel::Hda },
                storage: DataAmount { quantity: 512, units: DataUnits::Megabytes },
                inputs: InputDevice { pointing_device: PointingDevice::Mouse },
                memory: DataAmount { quantity: 4, units: DataUnits::Gigabytes },
                window_size: Screen { height: 480, width: 640, units: ScreenUnits::Pixels },
            },
            start_up_args_template: Utf8PathBuf::from_path_buf(template_path.clone()).unwrap(),
            ports: None,
        };

        // Run the conversion, then assert everything in the config matches the manifest data.
        let config =
            convert_v2_bundle_to_configs(&pb, &device).expect("convert_v2_bundle_to_configs");
        assert_eq!(config.device.audio, device.hardware.audio);
        assert_eq!(config.device.cpu.architecture, device.hardware.cpu.arch);
        assert_eq!(config.device.memory, device.hardware.memory);
        assert_eq!(config.device.pointing_device, device.hardware.inputs.pointing_device);
        assert_eq!(config.device.screen, device.hardware.window_size);
        assert_eq!(config.device.storage, device.hardware.storage);

        assert!(config.guest.fvm_image.is_some());

        assert_eq!(config.guest.fvm_image.unwrap(), expected_fvm);
        assert_eq!(config.guest.kernel_image, expected_kernel);
        assert_eq!(config.guest.zbi_image, expected_zbi);

        assert_eq!(config.host.port_map.len(), 0);

        // Adjust all of the values that affect the config, then run it again.
        let expected_kernel = Utf8PathBuf::from_path_buf(sdk_root.join("some/new_kernel"))
            .expect("couldn't convert kernel to utf8");
        let expected_fvm = Utf8PathBuf::from_path_buf(sdk_root.join("another_fvm"))
            .expect("couldn't convert fvm to utf8");
        let expected_zbi = Utf8PathBuf::from_path_buf(sdk_root.join("path/to/new_zbi"))
            .expect("couldn't convert zbi to utf8");

        pb.system_a = Some(vec![
            Image::ZBI { path: expected_zbi.clone(), signed: false },
            Image::QemuKernel(expected_kernel.clone()),
            Image::FVM(expected_fvm.clone()),
        ]);
        device.hardware = Hardware {
            cpu: Cpu { arch: CpuArchitecture::Arm64 },
            audio: AudioDevice { model: AudioModel::None },
            storage: DataAmount { quantity: 8, units: DataUnits::Gigabytes },
            inputs: InputDevice { pointing_device: PointingDevice::Touch },
            memory: DataAmount { quantity: 2048, units: DataUnits::Megabytes },
            window_size: Screen { height: 1024, width: 1280, units: ScreenUnits::Pixels },
        };
        device.start_up_args_template = Utf8PathBuf::from_path_buf(template_path).unwrap();

        let mut ports = HashMap::new();
        ports.insert("ssh".to_string(), 22);
        ports.insert("debug".to_string(), 2345);
        device.ports = Some(ports);

        let mut config =
            convert_v2_bundle_to_configs(&pb, &device).expect("convert_bundle_v2_to_configs");

        // Verify that all of the new values are loaded and match the new manifest data.
        assert_eq!(config.device.audio, device.hardware.audio);
        assert_eq!(config.device.cpu.architecture, device.hardware.cpu.arch);
        assert_eq!(config.device.memory, device.hardware.memory);
        assert_eq!(config.device.pointing_device, device.hardware.inputs.pointing_device);
        assert_eq!(config.device.screen, device.hardware.window_size);
        assert_eq!(config.device.storage, device.hardware.storage);

        assert!(config.guest.fvm_image.is_some());

        assert_eq!(config.guest.fvm_image.unwrap(), expected_fvm);
        assert_eq!(config.guest.kernel_image, expected_kernel);
        assert_eq!(config.guest.zbi_image, expected_zbi);

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
}
