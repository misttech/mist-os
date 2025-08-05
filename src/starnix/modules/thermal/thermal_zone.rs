// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_lock::OnceCell;
use fidl_fuchsia_hardware_temperature as ftemperature;
use std::sync::Arc;

/// A container for synchronous and asynchronous proxies for a temperature sensor.
///
/// The synchronous proxy is used for blocking read requests for temperature
/// values such as those from the sysfs thermal device class.
/// The asynchronous proxy is used in asynchronous contexts like handling
/// netlink multicast clients and scheduled tasks that fetch temperature data.
#[derive(Debug)]
pub struct SensorProxy {
    pub asynch: ftemperature::DeviceProxy,
    pub sync: ftemperature::DeviceSynchronousProxy,
}

#[derive(Clone)]
pub struct ThermalZone {
    pub id: u32,
    pub proxy: Arc<OnceCell<SensorProxy>>,
}
