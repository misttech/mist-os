// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/goldfish-display/display-driver.h"

#include <fidl/fuchsia.hardware.goldfish/cpp/wire.h>
#include <fidl/fuchsia.hardware.sysmem/cpp/wire.h>
#include <fidl/fuchsia.sysmem/cpp/wire.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/driver.h>
#include <lib/zx/result.h>
#include <zircon/errors.h>
#include <zircon/process.h>
#include <zircon/status.h>

#include <memory>
#include <string_view>

#include <fbl/alloc_checker.h>

#include "src/graphics/display/drivers/goldfish-display/display-engine.h"
#include "src/graphics/display/drivers/goldfish-display/render_control.h"

namespace goldfish {
namespace {

zx_koid_t GetKoid(zx_handle_t handle) {
  zx_info_handle_basic_t info;
  zx_status_t status =
      zx_object_get_info(handle, ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr);
  return status == ZX_OK ? info.koid : ZX_KOID_INVALID;
}

zx::result<fidl::ClientEnd<fuchsia_sysmem::Allocator>> CreateAndInitializeSysmemAllocator(
    zx_device_t* parent) {
  zx::result<fidl::ClientEnd<fuchsia_sysmem::Allocator>> connect_sysmem_service_result =
      ddk::Device<void>::DdkConnectFidlProtocol<fuchsia_hardware_sysmem::Service::AllocatorV1>(
          parent);
  if (connect_sysmem_service_result.is_error()) {
    zxlogf(ERROR, "Failed to connect to the sysmem Allocator FIDL protocol: %s",
           connect_sysmem_service_result.status_string());
    return connect_sysmem_service_result.take_error();
  }
  fidl::ClientEnd<fuchsia_sysmem::Allocator> sysmem_allocator =
      std::move(connect_sysmem_service_result).value();

  const zx_koid_t pid = GetKoid(zx_process_self());
  static constexpr std::string_view kDebugName = "goldfish-display";
  fidl::OneWayStatus set_debug_status =
      fidl::WireCall(sysmem_allocator)
          ->SetDebugClientInfo(fidl::StringView::FromExternal(kDebugName), pid);
  if (!set_debug_status.ok()) {
    zxlogf(ERROR, "Failed to set sysmem allocator debug info: %s",
           set_debug_status.status_string());
    return zx::error(set_debug_status.status());
  }

  return zx::ok(std::move(sysmem_allocator));
}

zx::result<std::unique_ptr<RenderControl>> CreateAndInitializeRenderControl(zx_device_t* parent) {
  zx::result<fidl::ClientEnd<fuchsia_hardware_goldfish_pipe::GoldfishPipe>>
      render_control_connect_pipe_service_result = ddk::Device<void>::DdkConnectFidlProtocol<
          fuchsia_hardware_goldfish_pipe::Service::Device>(parent);
  if (render_control_connect_pipe_service_result.is_error()) {
    zxlogf(ERROR, "Failed to connect to the goldfish pipe FIDL service: %s",
           render_control_connect_pipe_service_result.status_string());
    return render_control_connect_pipe_service_result.take_error();
  }
  fidl::ClientEnd<fuchsia_hardware_goldfish_pipe::GoldfishPipe> render_control_pipe =
      std::move(render_control_connect_pipe_service_result).value();

  fbl::AllocChecker alloc_checker;
  auto render_control = fbl::make_unique_checked<RenderControl>(&alloc_checker);
  if (!alloc_checker.check()) {
    zxlogf(ERROR, "Failed to allocate memory for RenderControl");
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  zx_status_t status =
      render_control->InitRcPipe(fidl::WireSyncClient(std::move(render_control_pipe)));
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to initialize RenderControl: %d", status);
    return zx::error(status);
  }

  return zx::ok(std::move(render_control));
}

}  // namespace

// static
zx::result<> DisplayDriver::Create(zx_device_t* parent) {
  ZX_DEBUG_ASSERT(parent != nullptr);

  zx::result<fidl::ClientEnd<fuchsia_hardware_goldfish::ControlDevice>>
      connect_control_service_result =
          DdkConnectFidlProtocol<fuchsia_hardware_goldfish::ControlService::Device>(parent);
  if (connect_control_service_result.is_error()) {
    zxlogf(ERROR, "Failed to connect to the goldfish Control FIDL service: %s",
           connect_control_service_result.status_string());
    return connect_control_service_result.take_error();
  }
  fidl::ClientEnd<fuchsia_hardware_goldfish::ControlDevice> control =
      std::move(connect_control_service_result).value();

  zx::result<fidl::ClientEnd<fuchsia_hardware_goldfish_pipe::GoldfishPipe>>
      connect_pipe_service_result =
          DdkConnectFidlProtocol<fuchsia_hardware_goldfish_pipe::Service::Device>(parent);
  if (connect_pipe_service_result.is_error()) {
    zxlogf(ERROR, "Failed to connect to the goldfish pipe FIDL service: %s",
           connect_pipe_service_result.status_string());
    return connect_pipe_service_result.take_error();
  }
  fidl::ClientEnd<fuchsia_hardware_goldfish_pipe::GoldfishPipe> pipe =
      std::move(connect_pipe_service_result).value();

  zx::result<fidl::ClientEnd<fuchsia_sysmem::Allocator>> create_sysmem_allocator_result =
      CreateAndInitializeSysmemAllocator(parent);
  if (create_sysmem_allocator_result.is_error()) {
    zxlogf(ERROR, "Failed to create and initialize sysmem allocator: %s",
           create_sysmem_allocator_result.status_string());
    return create_sysmem_allocator_result.take_error();
  }
  fidl::ClientEnd<fuchsia_sysmem::Allocator> sysmem_allocator =
      std::move(create_sysmem_allocator_result).value();

  zx::result<std::unique_ptr<RenderControl>> create_render_control_result =
      CreateAndInitializeRenderControl(parent);
  if (create_render_control_result.is_error()) {
    zxlogf(ERROR, "Failed to create and initialize RenderControl: %s",
           create_render_control_result.status_string());
    return create_render_control_result.take_error();
  }
  std::unique_ptr<RenderControl> render_control = std::move(create_render_control_result).value();

  fbl::AllocChecker alloc_checker;
  auto display_engine = fbl::make_unique_checked<DisplayEngine>(
      &alloc_checker, std::move(control), std::move(pipe), std::move(sysmem_allocator),
      std::move(render_control));
  if (!alloc_checker.check()) {
    zxlogf(ERROR, "Failed to allocate memory for DisplayEngine");
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  zx::result<> init_result = display_engine->Initialize();
  if (init_result.is_error()) {
    zxlogf(ERROR, "Failed to initialize DisplayEngine: %s", init_result.status_string());
    return init_result.take_error();
  }

  auto display_driver =
      fbl::make_unique_checked<DisplayDriver>(&alloc_checker, parent, std::move(display_engine));
  if (!alloc_checker.check()) {
    zxlogf(ERROR, "Failed to allocate memory for DisplayDriver");
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  zx::result<> bind_result = display_driver->Bind();
  if (bind_result.is_error()) {
    zxlogf(ERROR, "Failed to bind goldfish-display: %s", bind_result.status_string());
    return bind_result.take_error();
  }

  // Device manager now owns `display_driver`.
  [[maybe_unused]] auto* display_driver_released = display_driver.release();
  return zx::ok();
}

DisplayDriver::DisplayDriver(zx_device_t* parent, std::unique_ptr<DisplayEngine> display_engine)
    : DisplayType(parent), display_engine_(std::move(display_engine)) {
  ZX_DEBUG_ASSERT(parent != nullptr);
  ZX_DEBUG_ASSERT(display_engine_ != nullptr);
}

DisplayDriver::~DisplayDriver() = default;

void DisplayDriver::DdkRelease() { delete this; }

zx_status_t DisplayDriver::DdkGetProtocol(uint32_t proto_id, void* out) {
  ZX_DEBUG_ASSERT(display_engine_ != nullptr);

  switch (proto_id) {
    case ZX_PROTOCOL_DISPLAY_CONTROLLER_IMPL: {
      auto* proto = static_cast<ddk::AnyProtocol*>(out);
      proto->ctx = display_engine_.get();
      proto->ops = display_engine_->display_controller_impl_protocol_ops();
      return ZX_OK;
    }
    default:
      return ZX_ERR_NOT_SUPPORTED;
  }
}

zx::result<> DisplayDriver::Bind() {
  zx_status_t status = DdkAdd(
      ddk::DeviceAddArgs("goldfish-display").set_proto_id(ZX_PROTOCOL_DISPLAY_CONTROLLER_IMPL));
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to add goldfish-display node: %s", zx_status_get_string(status));
    return zx::error(status);
  }

  return zx::ok();
}

}  // namespace goldfish

static constexpr zx_driver_ops_t kGoldfishDisplayDriverOps = []() -> zx_driver_ops_t {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = [](void* ctx, zx_device_t* parent) {
    zx::result<> result = goldfish::DisplayDriver::Create(parent);
    return result.status_value();
  };
  return ops;
}();

ZIRCON_DRIVER(goldfish_display, kGoldfishDisplayDriverOps, "zircon", "0.1");
