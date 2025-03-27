// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "aml-trip.h"

#include <fidl/fuchsia.driver.framework/cpp/wire_types.h>
#include <fidl/fuchsia.hardware.clock/cpp/wire.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.device/cpp/wire.h>
#include <fidl/fuchsia.hardware.trippoint/cpp/common_types.h>
#include <fidl/fuchsia.hardware.trippoint/cpp/markers.h>
#include <fidl/fuchsia.hardware.trippoint/cpp/wire_messaging.h>
#include <fidl/fuchsia.hardware.trippoint/cpp/wire_types.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/driver/platform-device/cpp/pdev.h>
#include <lib/fidl/cpp/wire/arena.h>
#include <lib/fidl/cpp/wire/channel.h>
#include <lib/fidl/cpp/wire/wire_messaging_declarations.h>
#include <lib/mmio/mmio-buffer.h>
#include <lib/zx/result.h>
#include <zircon/assert.h>
#include <zircon/errors.h>
#include <zircon/status.h>

#include <memory>
#include <optional>

#include <src/devices/temperature/drivers/aml-trip/aml-trip-device.h>

#include "lib/driver/compat/cpp/metadata.h"
#include "src/devices/temperature/drivers/aml-trip/util.h"

namespace temperature {

static constexpr char kDeviceName[] = "aml-trip-device";

zx::result<> AmlTrip::Start() {
  fidl::Arena arena;
  std::optional<TemperatureCelsius> critical_temperature = std::nullopt;
  zx::result decoded = compat::GetMetadata<fuchsia_hardware_trippoint::wire::TripDeviceMetadata>(
      incoming(), arena, DEVICE_METADATA_TRIP);
  if (decoded.is_error()) {
    if (decoded.status_value() != ZX_ERR_NOT_FOUND) {
      FDF_LOG(ERROR, "Failed to get trip sensor metadata: %s", decoded.status_string());
      return zx::error(decoded.status_value());
    }
  } else {
    critical_temperature = decoded->critical_temp_celsius;
  }

  zx::result pdev_client = incoming()->Connect<fuchsia_hardware_platform_device::Service::Device>();
  if (pdev_client.is_error() || !pdev_client->is_valid()) {
    FDF_LOG(ERROR, "Failed to connect to platform device: %s", pdev_client.status_string());
    return pdev_client.take_error();
  }
  fdf::PDev pdev{std::move(pdev_client.value())};

  // Stash a name for this device to be returned by `GetSensorName`
  zx::result device_info_result = pdev.GetDeviceInfo();
  if (device_info_result.is_error()) {
    FDF_LOG(ERROR, "Failed to get device info: %s", device_info_result.status_string());
    return device_info_result.take_error();
  }
  fdf::PDev::DeviceInfo device_info = std::move(device_info_result.value());

  std::string name = device_info.name;

  zx::result sensor_mmio = pdev.MapMmio(kSensorMmioIndex);
  if (sensor_mmio.is_error()) {
    FDF_LOG(ERROR, "Failed to map sensor mmio: %s", sensor_mmio.status_string());
    return sensor_mmio.take_error();
  }

  zx::result trim_mmio = pdev.MapMmio(kTrimMmioIndex);
  if (trim_mmio.is_error()) {
    FDF_LOG(ERROR, "Failed to map trim mmio: %s", trim_mmio.status_string());
    return trim_mmio.take_error();
  }

  const uint32_t trim_info = trim_mmio->Read32(0);

  zx::result irq = pdev.GetInterrupt(0);
  if (irq.is_error()) {
    FDF_LOG(ERROR, "Failed to get sensor interrupt: %s", irq.status_string());
    return irq.take_error();
  }

  device_ = std::make_unique<AmlTripDevice>(dispatcher(), trim_info, name,
                                            std::move(sensor_mmio.value()), std::move(irq.value()));
  device_->Init();

  if (critical_temperature) {
    FDF_LOG(INFO, "Configuring critical temperature for '%s' at %0.2fC", name.c_str(),
            *critical_temperature);
    device_->SetRebootTemperatureCelsius(*critical_temperature);
  }

  auto result = outgoing()->component().AddUnmanagedProtocol<fuchsia_hardware_trippoint::TripPoint>(
      trippoint_bindings_.CreateHandler(device_.get(), dispatcher(), fidl::kIgnoreBindingClosure),
      kDeviceName);
  if (result.is_error()) {
    FDF_LOG(ERROR, "Failed to add Device service %s", result.status_string());
    return result.take_error();
  }

  zx::result create_devfs_node_result = CreateDevfsNode();
  if (create_devfs_node_result.is_error()) {
    FDF_LOG(ERROR, "Failed to export to devfs %s", create_devfs_node_result.status_string());
    return create_devfs_node_result.take_error();
  }

  FDF_LOG(INFO, "Started Amlogic Trip Point Driver");

  return zx::ok();
}

void AmlTrip::Stop() {}

void AmlTrip::PrepareStop(fdf::PrepareStopCompleter completer) {
  device_->Shutdown();
  completer(zx::ok());
}

zx::result<> AmlTrip::CreateDevfsNode() {
  zx::result connector = devfs_connector_.Bind(dispatcher());
  if (connector.is_error()) {
    FDF_LOG(ERROR, "Error creating devfs node");
    return connector.take_error();
  }

  fuchsia_driver_framework::DevfsAddArgs devfs_args{
      {.connector = std::move(connector.value()),
       .class_name = "trippoint",
       .connector_supports = fuchsia_device_fs::ConnectionType::kController}};

  zx::result child = AddOwnedChild(kDeviceName, devfs_args);
  if (child.is_error()) {
    FDF_LOG(ERROR, "Failed to add owned child: %s", child.status_string());
    return child.take_error();
  }
  child_ = std::move(child.value());

  return zx::ok();
}

void AmlTrip::Serve(fidl::ServerEnd<fuchsia_hardware_trippoint::TripPoint> request) {
  trippoint_bindings_.AddBinding(dispatcher(), std::move(request), device_.get(),
                                 fidl::kIgnoreBindingClosure);
}

}  // namespace temperature

FUCHSIA_DRIVER_EXPORT(temperature::AmlTrip);
