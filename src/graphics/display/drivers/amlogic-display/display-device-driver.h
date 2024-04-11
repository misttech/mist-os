// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_DISPLAY_DEVICE_DRIVER_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_DISPLAY_DEVICE_DRIVER_H_

#include <lib/ddk/device.h>
#include <zircon/types.h>

#include <memory>

#include <ddktl/device.h>

#include "src/graphics/display/drivers/amlogic-display/display-engine.h"
#include "src/graphics/display/lib/driver-framework-migration-utils/dispatcher/dispatcher-factory.h"
#include "src/graphics/display/lib/driver-framework-migration-utils/metadata/metadata-getter.h"
#include "src/graphics/display/lib/driver-framework-migration-utils/namespace/namespace.h"

namespace amlogic_display {

class DisplayDeviceDriver;
using DeviceType = ddk::Device<DisplayDeviceDriver, ddk::GetProtocolable>;

// Integration between this driver and the Driver Framework (v1).
class DisplayDeviceDriver final : public DeviceType {
 public:
  // Factory method used by the device manager glue code.
  //
  // `parent` must not be null.
  static zx_status_t Create(zx_device_t* parent);

  // Exposed for testing. Production code should use the `Create()` factory
  // method instead.
  //
  // `incoming` must outlive `display_engine`.
  // `metadata_getter` must outlive `display_engine`.
  // `dispatcher_factory` must outlive `display_engine`.
  explicit DisplayDeviceDriver(zx_device_t* parent, std::unique_ptr<display::Namespace> incoming,
                               std::unique_ptr<display::MetadataGetter> metadata_getter,
                               std::unique_ptr<display::DispatcherFactory> dispatcher_factory,
                               std::unique_ptr<DisplayEngine> display_engine);

  DisplayDeviceDriver(const DisplayDeviceDriver&) = delete;
  DisplayDeviceDriver(DisplayDeviceDriver&&) = delete;
  DisplayDeviceDriver& operator=(const DisplayDeviceDriver&) = delete;
  DisplayDeviceDriver& operator=(DisplayDeviceDriver&&) = delete;

  ~DisplayDeviceDriver();

  // Resource initialization that is not suitable for the constructor.
  zx::result<> Init();

  // ddk::Device interface.
  void DdkRelease();

  // ddk::GetProtocolable interface.
  zx_status_t DdkGetProtocol(uint32_t proto_id, void* out_protocol);

 private:
  // `incoming_` must outlive `display_engine_`.
  std::unique_ptr<display::Namespace> incoming_;
  // `metadata_getter_` must outlive `display_engine_`.
  std::unique_ptr<display::MetadataGetter> metadata_getter_;
  // `dispatcher_factory_` must outlive `display_engine_`.
  std::unique_ptr<display::DispatcherFactory> dispatcher_factory_;
  std::unique_ptr<DisplayEngine> display_engine_;
};

}  // namespace amlogic_display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_DISPLAY_DEVICE_DRIVER_H_
