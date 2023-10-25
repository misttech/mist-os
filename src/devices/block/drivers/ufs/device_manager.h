// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BLOCK_DRIVERS_UFS_DEVICE_MANAGER_H_
#define SRC_DEVICES_BLOCK_DRIVERS_UFS_DEVICE_MANAGER_H_

#include <lib/trace/event.h>
#include <lib/zx/result.h>

#include "src/devices/block/drivers/ufs/upiu/attributes.h"
#include "src/devices/block/drivers/ufs/upiu/descriptors.h"
#include "src/devices/block/drivers/ufs/upiu/flags.h"

namespace ufs {

// UFS Specification Version 3.1, section 5.1.2 "UFS Device Manager"
// The device manager has the following two responsibilities:
// - Handling device level operations.
// - Managing device level configurations.
// Device level operations include functions such as device power management, settings related to
// data transfer, background operations enabling, and other device specific operations.
// Device level configuration is managed by the device manager by maintaining and storing a set of
// descriptors. The device manager handles commands like query request which allow to modify or
// retrieve configuration information of the device.

// Query requests and link-layer control should be sent from the DeviceManager.
class Ufs;
class DeviceManager {
 public:
  static zx::result<std::unique_ptr<DeviceManager>> Create(Ufs &controller);
  explicit DeviceManager(Ufs &controller) : controller_(controller) {}

  // Device initialization.
  zx::result<> SendLinkStartUp();
  zx::result<> DeviceInit();
  zx::result<> CheckBootLunEnabled();
  zx::result<> GetControllerDescriptor();
  zx::result<UnitDescriptor> ReadUnitDescriptor(uint8_t lun);

  // Device power management.
  zx::result<> SetReferenceClock();
  zx::result<> SetUicPowerMode();

  // for test
  DeviceDescriptor &GetDeviceDescriptor() { return device_descriptor_; }
  GeometryDescriptor &GetGeometryDescriptor() { return geometry_descriptor_; }

 private:
  friend class UfsTest;

  Ufs &controller_;

  DeviceDescriptor device_descriptor_;
  GeometryDescriptor geometry_descriptor_;
};

}  // namespace ufs

#endif  // SRC_DEVICES_BLOCK_DRIVERS_UFS_DEVICE_MANAGER_H_
