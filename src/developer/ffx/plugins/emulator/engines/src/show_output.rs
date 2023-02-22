// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! The show_output module contains print routines for the Show subcommand.

use camino::Utf8PathBuf;
use emulator_instance::{EmulatorConfiguration, NetworkingMode};
use ffx_emulator_common::tuntap::TAP_INTERFACE_NAME;
use sdk_metadata::{
    virtual_device::{Cpu, Hardware},
    ElementType, InputDevice, VirtualDeviceV1,
};
use serde_json;
use std::collections::HashMap;

pub(crate) fn net(emu_config: &EmulatorConfiguration) {
    if emu_config.runtime.config_override {
        println!(
            "Configuration was provided manually to the start command using the --config flag.\n\
            Network details for this instance cannot be shown with this tool; try\n    \
                `ffx emu show --config`\n\
            to review the emulator flags directly."
        );
        return;
    }
    println!("Networking Mode: {}", emu_config.host.networking);
    match emu_config.host.networking {
        NetworkingMode::Tap => {
            println!("  MAC: {}", emu_config.runtime.mac_address);
            println!("  Interface: {}", TAP_INTERFACE_NAME);
            if emu_config.runtime.upscript.is_some() {
                println!(
                    "  Upscript: {}", 
                    emu_config.runtime.upscript.as_ref().unwrap().display()
                );
            }
        }
        NetworkingMode::User => {
            println!("  MAC: {}", emu_config.runtime.mac_address);
            println!("  Ports:");
            for name in emu_config.host.port_map.keys() {
                let ports = emu_config.host.port_map.get(name).unwrap();
                println!(
                    "    {}:\n      guest: {}\n      host: {}", 
                    name,
                    ports.guest,
                    // Every port in the map must be assigned before start-up.
                    ports.host.unwrap(),
                )
            }
        }
        NetworkingMode::Auto |  /* Auto will already be resolved, so skip */
        NetworkingMode::None => /* nothing to add, networking is disabled */ (),
    }
}

pub(crate) fn config(emu_config: &EmulatorConfiguration) {
    match serde_json::to_string_pretty(&emu_config.flags) {
        Ok(flags) => println!("{}", flags),
        Err(e) => eprintln!("{:?}", e),
    }
}

pub(crate) fn device(emu_config: &EmulatorConfiguration) {
    let mut device = VirtualDeviceV1 {
        name: format!("{}_device", emu_config.runtime.name),
        description: Some(format!(
            "The virtual device used to launch the {} emulator.",
            emu_config.runtime.name
        )),
        kind: ElementType::VirtualDevice,
        hardware: Hardware {
            cpu: Cpu { arch: emu_config.device.cpu.architecture.clone() },
            audio: emu_config.device.audio.clone(),
            storage: emu_config.device.storage.clone(),
            inputs: InputDevice { pointing_device: emu_config.device.pointing_device.clone() },
            memory: emu_config.device.memory.clone(),
            window_size: emu_config.device.screen.clone(),
        },
        start_up_args_template: Utf8PathBuf::from_path_buf(emu_config.runtime.template.clone())
            .expect("Converting to utf8"),
        ports: None,
    };

    let mut ports = HashMap::new();
    for (name, mapping) in &emu_config.host.port_map {
        ports.insert(name.clone(), mapping.guest);
    }
    if !ports.is_empty() {
        device.ports = Some(ports.clone());
    }

    match serde_json::to_string_pretty(&device) {
        Ok(text) => println!("{}", text),
        Err(e) => eprintln!("{:?}", e),
    }
}
