// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Result;
use fastboot::{
    command::{ClientVariable, Command},
    reply::Reply,
    send,
};
use futures::lock::Mutex;
use lazy_static::lazy_static;
use std::collections::HashSet;
use usb_bulk::{AsyncInterface as Interface, InterfaceInfo, Open};

lazy_static! {
    /// This is used to lock serial numbers that are in use from being interrupted by the discovery
    /// loop.  Otherwise, it's possible to read the REPLY for the discovery code in the flashing
    /// workflow.
    /// Other processes are prevented interacting with the device due to the fact that the
    /// interface is claimed as long as it is open. So this Mutex only needs to implement exclusive
    /// access within the process.
    static ref SERIALS_IN_USE: Mutex<HashSet<String>> = Mutex::new(HashSet::new());
}

pub async fn global_serials_update<F>(f: F)
where
    F: FnOnce(&mut HashSet<String>),
{
    let mut in_use = SERIALS_IN_USE.lock().await;
    f(&mut *in_use)
}

// USB fastboot interface IDs
const FASTBOOT_AND_CDC_ETH_USB_DEV_PRODUCT: u16 = 0xa027;
const FASTBOOT_USB_INTERFACE_CLASS: u8 = 0xff;
const FASTBOOT_USB_INTERFACE_SUBCLASS: u8 = 0x42;
const FASTBOOT_USB_INTERFACE_PROTOCOL: u8 = 0x03;

//TODO(fxbug.dev/52733) - this info will probably get rolled into the target struct
#[derive(Debug)]
pub struct FastbootDevice {
    pub product: String,
    pub serial: String,
}

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

fn find_serial_numbers() -> Vec<String> {
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
pub fn open_interface_with_serial(serial: &String) -> Result<Interface> {
    tracing::debug!("Opening USB fastboot interface with serial number: {}", serial);
    open_interface(|info: &InterfaceInfo| -> bool { extract_serial_number(info) == *serial })
}

pub async fn find_devices() -> Vec<FastbootDevice> {
    let mut products = Vec::new();

    tracing::debug!("Discovering fastboot devices via usb");

    let serials = find_serial_numbers();
    let in_use = SERIALS_IN_USE.lock().await;
    // Don't probe in-use clients
    let serials_to_probe = serials.into_iter().filter(|s| !in_use.contains(s));
    for serial in serials_to_probe {
        match open_interface_with_serial(&serial) {
            Ok(mut usb_interface) => {
                if let Ok(Reply::Okay(version)) =
                    send(Command::GetVar(ClientVariable::Version), &mut usb_interface).await
                {
                    // Only support 0.4 right now.
                    if version == "0.4".to_string() {
                        if let Ok(Reply::Okay(product)) =
                            send(Command::GetVar(ClientVariable::Product), &mut usb_interface).await
                        {
                            products.push(FastbootDevice { product, serial: serial.to_string() })
                        }
                    }
                }
            }
            Err(e) => tracing::error!("Error opening USB interface: {}", e),
        }
    }
    products
}
