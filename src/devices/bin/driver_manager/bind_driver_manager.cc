// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/bind_driver_manager.h"

#include <errno.h>
#include <lib/fit/function.h>
#include <zircon/status.h>

#include "src/devices/bin/driver_manager/coordinator.h"
#include "src/devices/bin/driver_manager/v1/composite_node_spec_v1.h"
#include "src/devices/lib/log/log.h"

BindDriverManager::BindDriverManager(Coordinator* coordinator) : coordinator_(coordinator) {}

BindDriverManager::~BindDriverManager() {}

zx_status_t BindDriverManager::BindDriverToDevice(const MatchedDriver& driver,
                                                  const fbl::RefPtr<Device>& dev) {
  if (auto info = std::get_if<fdi::MatchedNodeRepresentationInfo>(&driver); info) {
    auto device_ptr = std::shared_ptr<DeviceV1Wrapper>(new DeviceV1Wrapper{
        .device = dev,
    });
    return coordinator_->node_group_manager()
        .BindNodeRepresentation(*info, device_ptr)
        .status_value();
  }

  if (!std::holds_alternative<MatchedDriverInfo>(driver)) {
    return ZX_ERR_INTERNAL;
  }
  auto driver_info = std::get<MatchedDriverInfo>(driver);
  zx_status_t status = coordinator_->AttemptBind(driver_info, dev);

  // If we get this here it means we've successfully bound one driver
  // and the device isn't multi-bind.
  if (status == ZX_ERR_ALREADY_BOUND) {
    return ZX_OK;
  }

  if (status != ZX_OK) {
    LOGF(ERROR, "Failed to bind driver '%s' to device '%.*s': %s", driver_info.name(),
         static_cast<uint32_t>(dev->name().size()), dev->name().data(),
         zx_status_get_string(status));
  }
  return status;
}

zx_status_t BindDriverManager::BindDevice(const fbl::RefPtr<Device>& dev) {
  // shouldn't be possible to get a bind request for a proxy device
  if (dev->flags & DEV_CTX_PROXY) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  // This is a general request, so skip devices that don't allow autobind.
  if (dev->flags & DEV_CTX_SKIP_AUTOBIND) {
    return ZX_OK;
  }

  // Attempt composite device matching first.
  for (auto& composite : coordinator_->device_manager()->composite_devices()) {
    auto status = composite.TryMatchBindFragments(dev);
    if (status != ZX_OK) {
      return status;
    }
  }

  return BindDriverToDevice(dev, "");
}

zx_status_t BindDriverManager::BindDriverToDevice(const fbl::RefPtr<Device>& dev,
                                                  std::string_view drvlibname) {
  // It shouldn't be possible to get a bind request for a proxy device.
  if (dev->flags & DEV_CTX_PROXY) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  // TODO: disallow if we're in the middle of enumeration, etc
  zx::result<std::vector<MatchedDriver>> result = GetMatchingDrivers(dev, drvlibname);
  if (!result.is_ok()) {
    return result.error_value();
  }

  auto drivers = std::move(result.value());
  for (auto& driver : drivers) {
    zx_status_t status = BindDriverToDevice(driver, dev);
    if (status != ZX_OK) {
      return status;
    }
  }

  return ZX_OK;
}

zx::result<std::vector<MatchedDriver>> BindDriverManager::GetMatchingDrivers(
    const fbl::RefPtr<Device>& dev, std::string_view drvlibname) {
  // It shouldn't be possible to get a bind request for a proxy device.
  if (dev->flags & DEV_CTX_PROXY) {
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  if (dev->IsAlreadyBound()) {
    return zx::error(ZX_ERR_ALREADY_BOUND);
  }

  // Check for drivers in the Driver-index.
  DriverLoader::MatchDeviceConfig config;
  config.libname = drvlibname;
  return zx::ok(coordinator_->driver_loader().MatchDeviceDriverIndex(dev, config));
}

zx::result<std::vector<MatchedDriver>> BindDriverManager::MatchDevice(
    const fbl::RefPtr<Device>& dev, const DriverLoader::MatchDeviceConfig& config) const {
  if (dev->IsAlreadyBound()) {
    return zx::error(ZX_ERR_ALREADY_BOUND);
  }

  if (dev->should_skip_autobind()) {
    return zx::error(ZX_ERR_NEXT);
  }

  if (!dev->is_bindable() && !(dev->is_composite_bindable())) {
    return zx::error(ZX_ERR_NEXT);
  }

  return zx::ok(coordinator_->driver_loader().MatchDeviceDriverIndex(dev, config));
}

zx_status_t BindDriverManager::MatchAndBind(const fbl::RefPtr<Device>& dev,
                                            const DriverLoader::MatchDeviceConfig& config) {
  auto result = MatchDevice(dev, config);
  if (!result.is_ok()) {
    return result.error_value();
  }

  auto matched_drivers = std::move(result.value());
  for (auto driver : matched_drivers) {
    zx_status_t status = BindDriverToDevice(driver, dev);

    // If we get this here it means we've successfully bound one driver
    // and the device isn't multi-bind.
    if (status == ZX_ERR_ALREADY_BOUND) {
      return ZX_OK;
    }
  }

  return ZX_OK;
}

zx::result<> BindDriverManager::MatchAndBindCompositeDevice(
    CompositeDevice& composite, const DriverLoader::MatchDeviceConfig& config) {
  if (composite.HasDriver()) {
    return zx::ok();
  }

  auto match_result = MatchCompositeDevice(composite, config);
  if (match_result.is_error()) {
    if (match_result.status_value() == ZX_ERR_NOT_FOUND) {
      return zx::ok();
    }
    return match_result.take_error();
  }

  composite.SetDriverAndAssemble(match_result.value());
  return zx::ok();
}

void BindDriverManager::BindAllDevices(const DriverLoader::MatchDeviceConfig& config) {
  for (auto& dev : coordinator_->device_manager()->devices()) {
    auto dev_ref = fbl::RefPtr(&dev);
    zx_status_t status = MatchAndBind(dev_ref, config);
    if (status == ZX_ERR_NEXT || status == ZX_ERR_ALREADY_BOUND) {
      continue;
    }
    if (status != ZX_OK) {
      return;
    }
  }

  for (auto& composite : coordinator_->device_manager()->composite_devices()) {
    auto result = MatchAndBindCompositeDevice(composite, config);
    if (result.is_error()) {
      LOGF(ERROR, "Failed to match and bind composite device'%s': %s", composite.name().data(),
           zx_status_get_string(result.status_value()));
    }
  }
}

zx_status_t BindDriverManager::MatchAndBindNodeGroups(const fbl::RefPtr<Device>& dev) {
  DriverLoader::MatchDeviceConfig config;
  auto result = MatchDevice(dev, config);
  if (!result.is_ok()) {
    if (result.error_value() == ZX_ERR_NEXT || result.error_value() == ZX_ERR_ALREADY_BOUND) {
      return ZX_OK;
    }
    return result.error_value();
  }

  auto matched_drivers = std::move(result.value());
  for (auto driver : matched_drivers) {
    if (!std::holds_alternative<fdi::MatchedNodeRepresentationInfo>(driver)) {
      continue;
    }

    auto device_ptr = std::shared_ptr<DeviceV1Wrapper>(new DeviceV1Wrapper{
        .device = dev,
    });
    auto bind_result = coordinator_->node_group_manager().BindNodeRepresentation(
        std::get<fdi::MatchedNodeRepresentationInfo>(driver), device_ptr);
    if (bind_result.is_error()) {
      LOGF(WARNING, "Failed to bind node group node: %d", bind_result.status_value());
    }
  }

  return ZX_OK;
}

zx::result<MatchedDriverInfo> BindDriverManager::MatchCompositeDevice(
    CompositeDevice& composite, const DriverLoader::MatchDeviceConfig& config) {
  auto matched_drivers = coordinator_->driver_loader().MatchDeviceDriverIndex(
      composite.properties(), composite.str_properties(), 0, config);
  for (auto driver : matched_drivers) {
    if (auto info = std::get_if<MatchedDriverInfo>(&driver); info) {
      return zx::ok(*info);
    }
  }

  return zx::error(ZX_ERR_NOT_FOUND);
}
