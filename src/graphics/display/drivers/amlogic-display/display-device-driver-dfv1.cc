// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/amlogic-display/display-device-driver-dfv1.h"

#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/fit/defer.h>
#include <lib/zx/result.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <memory>

#include <ddktl/device.h>
#include <fbl/alloc_checker.h>

#include "src/graphics/display/lib/driver-framework-migration-utils/dispatcher/dispatcher-factory.h"
#include "src/graphics/display/lib/driver-framework-migration-utils/dispatcher/loop-backed-dispatcher-factory.h"
#include "src/graphics/display/lib/driver-framework-migration-utils/metadata/metadata-getter-dfv1.h"
#include "src/graphics/display/lib/driver-framework-migration-utils/metadata/metadata-getter.h"
#include "src/graphics/display/lib/driver-framework-migration-utils/namespace/namespace-dfv1.h"
#include "src/graphics/display/lib/driver-framework-migration-utils/namespace/namespace.h"

namespace amlogic_display {

// static
zx_status_t DisplayDeviceDriverDfv1::Create(zx_device_t* parent) {
  zx::result<std::unique_ptr<display::Namespace>> create_incoming_result =
      display::NamespaceDfv1::Create(parent);
  if (create_incoming_result.is_error()) {
    zxlogf(ERROR, "Failed to create incoming display::Namespace: %s",
           create_incoming_result.status_string());
    return create_incoming_result.status_value();
  }
  std::unique_ptr<display::Namespace> incoming = std::move(create_incoming_result).value();

  zx::result<std::unique_ptr<display::MetadataGetter>> create_metadata_getter_result =
      display::MetadataGetterDfv1::Create(parent);
  if (create_metadata_getter_result.is_error()) {
    zxlogf(ERROR, "Failed to create Metadata getter: %s",
           create_metadata_getter_result.status_string());
    return create_metadata_getter_result.status_value();
  }
  std::unique_ptr<display::MetadataGetter> metadata_getter =
      std::move(create_metadata_getter_result).value();

  zx::result<std::unique_ptr<display::DispatcherFactory>> create_dispatcher_factory_result =
      display::LoopBackedDispatcherFactory::Create(parent);
  if (create_dispatcher_factory_result.is_error()) {
    zxlogf(ERROR, "Failed to create Dispatcher factory: %s",
           create_dispatcher_factory_result.status_string());
    return create_dispatcher_factory_result.status_value();
  }
  std::unique_ptr<display::DispatcherFactory> dispatcher_factory =
      std::move(create_dispatcher_factory_result).value();

  zx::result<std::unique_ptr<DisplayEngine>> display_engine_result =
      DisplayEngine::Create(incoming.get(), metadata_getter.get(), dispatcher_factory.get());
  if (display_engine_result.is_error()) {
    zxlogf(ERROR, "Failed to create DisplayEngine instance: %s",
           display_engine_result.status_string());
    return display_engine_result.status_value();
  }
  std::unique_ptr<DisplayEngine> display_engine = std::move(display_engine_result).value();
  auto cleanup =
      fit::defer([display_engine = display_engine.get()]() { display_engine->Deinitialize(); });

  fbl::AllocChecker alloc_checker;
  auto display_device_driver = fbl::make_unique_checked<DisplayDeviceDriverDfv1>(
      &alloc_checker, parent, std::move(incoming), std::move(metadata_getter),
      std::move(dispatcher_factory), std::move(display_engine));
  if (!alloc_checker.check()) {
    zxlogf(ERROR, "Failed to allocate memory for DisplayDeviceDriver");
    return ZX_ERR_NO_MEMORY;
  }

  zx::result<> init_result = display_device_driver->Init();
  if (init_result.is_error()) {
    zxlogf(ERROR, "Failed to add device: %s", init_result.status_string());
    return init_result.status_value();
  }
  // `display_device_driver` is managed by the device manager.
  [[maybe_unused]] DisplayDeviceDriverDfv1* display_device_driver_ptr =
      display_device_driver.release();

  cleanup.cancel();
  return ZX_OK;
}

DisplayDeviceDriverDfv1::DisplayDeviceDriverDfv1(
    zx_device_t* parent, std::unique_ptr<display::Namespace> incoming,
    std::unique_ptr<display::MetadataGetter> metadata_getter,
    std::unique_ptr<display::DispatcherFactory> dispatcher_factory,
    std::unique_ptr<DisplayEngine> display_engine)
    : DeviceType(parent),
      incoming_(std::move(incoming)),
      metadata_getter_(std::move(metadata_getter)),
      dispatcher_factory_(std::move(dispatcher_factory)),
      display_engine_(std::move(display_engine)) {}

DisplayDeviceDriverDfv1::~DisplayDeviceDriverDfv1() = default;

zx::result<> DisplayDeviceDriverDfv1::Init() {
  const ddk::DeviceAddArgs args = ddk::DeviceAddArgs("amlogic-display")
                                      .set_proto_id(ZX_PROTOCOL_DISPLAY_CONTROLLER_IMPL)
                                      .set_flags(DEVICE_ADD_ALLOW_MULTI_COMPOSITE)
                                      .set_inspect_vmo(display_engine_->inspector().DuplicateVmo());
  return zx::make_result(DdkAdd(args));
}

void DisplayDeviceDriverDfv1::DdkRelease() {
  display_engine_->Deinitialize();
  delete this;
}

zx_status_t DisplayDeviceDriverDfv1::DdkGetProtocol(uint32_t proto_id, void* out_protocol) {
  ZX_DEBUG_ASSERT(display_engine_ != nullptr);
  auto* proto = static_cast<ddk::AnyProtocol*>(out_protocol);
  proto->ctx = display_engine_.get();
  switch (proto_id) {
    case ZX_PROTOCOL_DISPLAY_CONTROLLER_IMPL:
      proto->ops = display_engine_->display_controller_impl_protocol_ops();
      return ZX_OK;
    default:
      return ZX_ERR_NOT_SUPPORTED;
  }
}

namespace {

constexpr zx_driver_ops_t kDriverOps = {
    .version = DRIVER_OPS_VERSION,
    .bind = [](void* ctx, zx_device_t* parent) { return DisplayDeviceDriverDfv1::Create(parent); },
};

}  // namespace

}  // namespace amlogic_display

// clang-format off
ZIRCON_DRIVER(amlogic_display, amlogic_display::kDriverOps, "zircon", "0.1");
