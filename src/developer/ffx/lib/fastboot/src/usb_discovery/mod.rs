// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::{bail, Result};
use fastboot::{
    command::{ClientVariable, Command},
    reply::Reply,
    send,
};
use usb_bulk::{AsyncInterface as Interface, InterfaceInfo, Open};

// USB fastboot interface IDs
const FASTBOOT_AND_CDC_ETH_USB_DEV_PRODUCT: u16 = 0xa027;
const FASTBOOT_USB_INTERFACE_CLASS: u8 = 0xff;
const FASTBOOT_USB_INTERFACE_SUBCLASS: u8 = 0x42;
const FASTBOOT_USB_INTERFACE_PROTOCOL: u8 = 0x03;

//TODO(fxbug.dev/52733) - serial info will probably get rolled into the target struct

fn is_fastboot_match(info: &InterfaceInfo) -> bool {
    (info.dev_vendor == 0x18d1)
        && ((info.dev_product == 0x4ee0)
            || (info.dev_product == 0x0d02)
            || (info.dev_product == FASTBOOT_AND_CDC_ETH_USB_DEV_PRODUCT))
        && (info.ifc_class == FASTBOOT_USB_INTERFACE_CLASS)
        && (info.ifc_subclass == FASTBOOT_USB_INTERFACE_SUBCLASS)
        && (info.ifc_protocol == FASTBOOT_USB_INTERFACE_PROTOCOL)
}

fn enumerate_interfaces<F>(mut cb: F)
where
    F: FnMut(&InterfaceInfo),
{
    tracing::debug!("Enumerating USB fastboot interfaces");
    let mut cb = |info: &InterfaceInfo| -> bool {
        if is_fastboot_match(info) {
            cb(info)
        }
        // Do not open anything.
        false
    };
    let _result = Interface::open(&mut cb);
}

pub fn find_serial_numbers() -> Vec<String> {
    let mut serials = Vec::new();
    let cb = |info: &InterfaceInfo| serials.push(extract_serial_number(info));
    enumerate_interfaces(cb);
    serials
}

fn open_interface<F>(mut cb: F) -> Result<Interface>
where
    F: FnMut(&InterfaceInfo) -> bool,
{
    tracing::debug!("Selecting USB fastboot interface to open");

    let mut open_cb = |info: &InterfaceInfo| -> bool {
        if is_fastboot_match(info) {
            cb(info)
        } else {
            // Do not open.
            false
        }
    };
    Interface::open(&mut open_cb).map_err(Into::into)
}

fn extract_serial_number(info: &InterfaceInfo) -> String {
    let null_pos = match info.serial_number.iter().position(|&c| c == 0) {
        Some(p) => p,
        None => {
            return "".to_string();
        }
    };
    (*String::from_utf8_lossy(&info.serial_number[..null_pos])).to_string()
}

#[tracing::instrument]
pub async fn open_interface_with_serial(serial: &str) -> Result<Interface> {
    tracing::debug!("Opening USB fastboot interface with serial number: {}", serial);
    let mut interface =
        open_interface(|info: &InterfaceInfo| -> bool { extract_serial_number(info) == *serial })?;
    if let Ok(Reply::Okay(version)) =
        send(Command::GetVar(ClientVariable::Version), &mut interface).await
    {
        // Only support 0.4 right now.
        if version == "0.4".to_string() {
            Ok(interface)
        } else {
            bail!(format!("USB serial {serial}: wrong version ({version})"))
        }
    } else {
        bail!(format!("USB serial {serial}: could not get version"))
    }
}
