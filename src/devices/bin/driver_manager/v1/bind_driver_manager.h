// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BIN_DRIVER_MANAGER_V1_BIND_DRIVER_MANAGER_H_
#define SRC_DEVICES_BIN_DRIVER_MANAGER_V1_BIND_DRIVER_MANAGER_H_

#include <lib/ddk/device.h>

#include <memory>

#include "src/devices/bin/driver_manager/v1/driver_loader.h"

class Coordinator;
class CompositeDevice;

using CompositeDeviceMap = std::unordered_map<std::string, std::unique_ptr<CompositeDevice>>;

class BindDriverManager {
 public:
  BindDriverManager(const BindDriverManager&) = delete;
  BindDriverManager& operator=(const BindDriverManager&) = delete;
  BindDriverManager(BindDriverManager&&) = delete;
  BindDriverManager& operator=(BindDriverManager&&) = delete;

  explicit BindDriverManager(Coordinator* coordinator);
  ~BindDriverManager();

  // Try binding a device. Returns ZX_ERR_ALREADY_BOUND if there
  // is a driver bound to the device and the device is not allowed to be bound multiple times.
  zx_status_t BindDevice(const fbl::RefPtr<Device>& dev);

  // Try binding a specific driver to the device. Returns ZX_ERR_ALREADY_BOUND if there
  // is a driver bound to the device and the device is not allowed to be bound multiple times.
  // Returns ZX_ERR_NOT_FOUND if `GetMatchingDrivers` doesn't return any drivers.
  zx_status_t BindDriverToDevice(const fbl::RefPtr<Device>& dev,
                                 std::string_view driver_url_suffix);

  // Binds all the devices to the drivers.
  void BindAllDevices(const DriverLoader::MatchDeviceConfig& config);

  // Find matching parents for |dev| and then bind them.
  zx_status_t MatchAndBindCompositeNodeSpec(const fbl::RefPtr<Device>& dev);

  // Finds a matching driver for |composite|.
  zx::result<MatchedDriverInfo> MatchCompositeDevice(CompositeDevice& composite,
                                                     const DriverLoader::MatchDeviceConfig& config);

 private:
  zx_status_t BindDriverToDevice(const MatchedDriver& driver, const fbl::RefPtr<Device>& dev);

  // Given a device, return all of the Drivers whose bind rules match with the device.
  // The returned vector is organized by priority, so if only one driver is being bound it
  // should be the first in the vector.
  // If |driver_url| is not empty then the device will only be checked against the driver
  // with that specific name.
  zx::result<std::vector<MatchedDriver>> GetMatchingDrivers(const fbl::RefPtr<Device>& dev,
                                                            std::string_view driver_url_suffix);

  // Find and return matching drivers for |dev|.
  zx::result<std::vector<MatchedDriver>> MatchDevice(
      const fbl::RefPtr<Device>& dev, const DriverLoader::MatchDeviceConfig& config) const;

  // Find matching drivers for |dev| and then bind them.
  zx_status_t MatchAndBind(const fbl::RefPtr<Device>& dev,
                           const DriverLoader::MatchDeviceConfig& config);

  // Finds and binds a matching driver to |composite|.
  zx::result<> MatchAndBindCompositeDevice(CompositeDevice& composite,
                                           const DriverLoader::MatchDeviceConfig& config);

  // Owner. Must outlive BindDriverManager.
  Coordinator* coordinator_;

  // All the composite devices received from the DriverIndex.
  // This maps driver URLs to the CompositeDevice object.
  CompositeDeviceMap driver_index_composite_devices_;
};

#endif  // SRC_DEVICES_BIN_DRIVER_MANAGER_V1_BIND_DRIVER_MANAGER_H_
