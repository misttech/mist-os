// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/simple/simple-display-driver.h"

#include <fidl/fuchsia.hardware.sysmem/cpp/wire.h>
#include <fidl/fuchsia.sysmem2/cpp/wire.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/driver.h>
#include <lib/mmio/mmio-buffer.h>

#include <memory>

#include <ddktl/device.h>
#include <fbl/alloc_checker.h>
#include <fbl/mutex.h>

#include "src/graphics/display/drivers/simple/simple-display.h"

namespace simple_display {

SimpleDisplayDriver::SimpleDisplayDriver(zx_device_t* parent, std::string device_name)
    : DeviceType(parent), device_name_(std::move(device_name)) {}

SimpleDisplayDriver::~SimpleDisplayDriver() = default;

void SimpleDisplayDriver::DdkRelease() { delete this; }

zx_status_t SimpleDisplayDriver::DdkGetProtocol(uint32_t proto_id, void* out) {
  ZX_DEBUG_ASSERT(simple_display_ != nullptr);

  auto* proto = static_cast<ddk::AnyProtocol*>(out);
  if (proto_id == ZX_PROTOCOL_DISPLAY_CONTROLLER_IMPL) {
    display_controller_impl_protocol_t display_controller_impl_protocol =
        simple_display_->GetProtocol();
    proto->ctx = display_controller_impl_protocol.ctx;
    proto->ops = display_controller_impl_protocol.ops;
    return ZX_OK;
  }
  return ZX_ERR_NOT_SUPPORTED;
}

zx::result<> SimpleDisplayDriver::Initialize() {
  zx::result<> configure_hardware_result = ConfigureHardware();
  if (configure_hardware_result.is_error()) {
    zxlogf(ERROR, "Failed to configure hardware: %s", configure_hardware_result.status_string());
    return configure_hardware_result.take_error();
  }

  zx::result<std::unique_ptr<SimpleDisplay>> simple_display_result =
      CreateAndInitializeSimpleDisplay();
  if (simple_display_result.is_error()) {
    zxlogf(ERROR, "Failed to create and initialize SimpleDisplay: %s",
           simple_display_result.status_string());
    return simple_display_result.take_error();
  }
  simple_display_ = std::move(simple_display_result).value();

  zx_status_t ddk_add_result = DdkAdd(
      ddk::DeviceAddArgs(device_name_.c_str()).set_proto_id(ZX_PROTOCOL_DISPLAY_CONTROLLER_IMPL));
  if (ddk_add_result != ZX_OK) {
    zxlogf(ERROR, "Failed to add SimpleDisplayDriver to ddk: %s",
           zx_status_get_string(ddk_add_result));
    return zx::error(ddk_add_result);
  }

  return zx::ok();
}

zx::result<std::unique_ptr<SimpleDisplay>> SimpleDisplayDriver::CreateAndInitializeSimpleDisplay() {
  zx::result<fdf::MmioBuffer> frame_buffer_mmio_result = GetFrameBufferMmioBuffer();
  if (frame_buffer_mmio_result.is_error()) {
    zxlogf(ERROR, "Failed to get frame buffer mmio buffer: %s",
           frame_buffer_mmio_result.status_string());
    return frame_buffer_mmio_result.take_error();
  }
  fdf::MmioBuffer frame_buffer_mmio = std::move(frame_buffer_mmio_result).value();

  zx::result<DisplayProperties> display_properties_result = GetDisplayProperties();
  if (display_properties_result.is_error()) {
    zxlogf(ERROR, "Failed to get display properties: %s",
           display_properties_result.status_string());
    return display_properties_result.take_error();
  }
  DisplayProperties display_properties = std::move(display_properties_result).value();

  zx::result<fidl::ClientEnd<fuchsia_hardware_sysmem::Sysmem>> hardware_sysmem_result =
      ddk::Device<void>::DdkConnectFragmentFidlProtocol<fuchsia_hardware_sysmem::Service::Sysmem>(
          parent(), "sysmem");
  if (hardware_sysmem_result.is_error()) {
    zxlogf(ERROR, "Failed to get hardware sysmem protocol: %s",
           hardware_sysmem_result.status_string());
    return hardware_sysmem_result.take_error();
  }
  fidl::WireSyncClient hardware_sysmem{std::move(hardware_sysmem_result).value()};

  zx::result<fidl::ClientEnd<fuchsia_sysmem2::Allocator>> sysmem_result =
      ddk::Device<void>::DdkConnectFragmentFidlProtocol<
          fuchsia_hardware_sysmem::Service::AllocatorV2>(parent(), "sysmem");
  if (sysmem_result.is_error()) {
    zxlogf(ERROR, "Failed to get fuchsia.sysmem2.Allocator protocol: %s",
           sysmem_result.status_string());
    return sysmem_result.take_error();
  }
  fidl::WireSyncClient sysmem(std::move(sysmem_result).value());

  fbl::AllocChecker alloc_checker;
  auto simple_display = fbl::make_unique_checked<SimpleDisplay>(
      &alloc_checker, std::move(hardware_sysmem), std::move(sysmem), std::move(frame_buffer_mmio),
      std::move(display_properties));
  if (!alloc_checker.check()) {
    zxlogf(ERROR, "Failed to allocate memory for SimpleDisplay");
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  zx::result<> simple_display_initialize_result = simple_display->Initialize();
  if (simple_display_initialize_result.is_error()) {
    zxlogf(ERROR, "Failed to initialize SimpleDisplay: %s",
           simple_display_initialize_result.status_string());
    return simple_display_initialize_result.take_error();
  }

  return zx::ok(std::move(simple_display));
}

}  // namespace simple_display
