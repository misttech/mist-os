// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/aml-canvas/aml-canvas-driver.h"

#include <fidl/fuchsia.driver.framework/cpp/wire.h>
#include <fidl/fuchsia.hardware.platform.device/cpp/wire.h>
#include <lib/driver/compat/cpp/logging.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/outgoing/cpp/outgoing_directory.h>
#include <lib/inspect/component/cpp/component.h>
#include <lib/inspect/cpp/inspector.h>
#include <lib/mmio/mmio-buffer.h>
#include <lib/zx/bti.h>
#include <lib/zx/result.h>
#include <zircon/assert.h>
#include <zircon/status.h>

#include <cinttypes>
#include <cstdint>
#include <memory>

#include <fbl/alloc_checker.h>

#include "src/graphics/display/drivers/aml-canvas/aml-canvas.h"
#include "src/graphics/display/drivers/aml-canvas/board-resources.h"

namespace aml_canvas {

AmlCanvasDriver::AmlCanvasDriver(fdf::DriverStartArgs start_args,
                                 fdf::UnownedSynchronizedDispatcher driver_dispatcher)
    : fdf::DriverBase("aml-canvas", std::move(start_args), std::move(driver_dispatcher)) {}

void AmlCanvasDriver::Stop() {
  fidl::OneWayStatus result = controller_->Remove();
  if (!result.ok()) {
    FDF_LOG(ERROR, "Failed to remove the Node: %s", result.status_string());
  }
}

zx::result<std::unique_ptr<inspect::ComponentInspector>>
AmlCanvasDriver::CreateComponentInspector() {
  zx::result<fidl::ClientEnd<fuchsia_inspect::InspectSink>> inspect_sink_connect_result =
      incoming()->Connect<fuchsia_inspect::InspectSink>();
  if (inspect_sink_connect_result.is_error()) {
    FDF_LOG(ERROR, "Failed to connect to InspectSink protocol: %s",
            inspect_sink_connect_result.status_string());
    return inspect_sink_connect_result.take_error();
  }

  fbl::AllocChecker alloc_checker;
  auto component_inspector = fbl::make_unique_checked<inspect::ComponentInspector>(
      &alloc_checker, fdf::Dispatcher::GetCurrent()->async_dispatcher(),
      inspect::PublishOptions{.inspector = inspect::Inspector{},
                              .client_end = std::move(inspect_sink_connect_result).value()});
  if (!alloc_checker.check()) {
    FDF_LOG(ERROR, "Failed to allocate memory for ComponentInspector");
    return zx::error(ZX_ERR_NO_MEMORY);
  }
  return zx::ok(std::move(component_inspector));
}

zx::result<std::unique_ptr<AmlCanvas>> AmlCanvasDriver::CreateAndServeCanvas(
    inspect::Inspector inspector) {
  zx::result<fidl::ClientEnd<fuchsia_hardware_platform_device::Device>> pdev_result =
      incoming()->Connect<fuchsia_hardware_platform_device::Service::Device>();
  if (pdev_result.is_error()) {
    FDF_LOG(ERROR, "Failed to connect to platform device: %s", pdev_result.status_string());
  }
  fidl::ClientEnd pdev(std::move(pdev_result).value());
  ZX_DEBUG_ASSERT(pdev.is_valid());

  zx::result<zx::bti> bti_result = GetBti(BtiResourceIndex::kCanvas, pdev);
  if (bti_result.is_error()) {
    FDF_LOG(ERROR, "Failed to get BTI from the platform device: %s", bti_result.status_string());
    return bti_result.take_error();
  }
  zx::bti bti = std::move(bti_result).value();

  zx::result<fdf::MmioBuffer> mmio_result = MapMmio(MmioResourceIndex::kDmc, pdev);
  if (mmio_result.is_error()) {
    FDF_LOG(ERROR, "Failed to map MMIO from the platform device: %s", mmio_result.status_string());
    return mmio_result.take_error();
  }
  fdf::MmioBuffer mmio = std::move(mmio_result).value();

  fbl::AllocChecker alloc_checker;
  auto canvas = fbl::make_unique_checked<AmlCanvas>(&alloc_checker, std::move(mmio), std::move(bti),
                                                    std::move(inspector));
  if (!alloc_checker.check()) {
    FDF_LOG(ERROR, "Failed to allocate AmlCanvas");
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  zx_status_t status = canvas->ServeOutgoing(outgoing());
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to serve to outgoing directory: %s", zx_status_get_string(status));
    return zx::error(status);
  }

  return zx::ok(std::move(canvas));
}

zx::result<fidl::ClientEnd<fuchsia_driver_framework::NodeController>> AmlCanvasDriver::AddChildNode(
    compat::SyncInitializedDeviceServer* compat_server) {
  fidl::Arena arena;
  std::vector<fuchsia_driver_framework::wire::Offer> offers;
  if (compat_server != nullptr) {
    offers = compat_server->CreateOffers2(arena);
  }
  offers.push_back(
      fdf::MakeOffer2<fuchsia_hardware_amlogiccanvas::Service>(arena, component::kDefaultInstance));

  auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena)
                  .name(arena, name())
                  .offers2(arena, std::move(offers))
                  .Build();

  zx::result<fidl::Endpoints<fuchsia_driver_framework::NodeController>>
      controller_endpoints_result =
          fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
  if (controller_endpoints_result.is_error()) {
    FDF_LOG(ERROR, "Failed to create endpoints: %s", controller_endpoints_result.status_string());
    return controller_endpoints_result.take_error();
  }
  auto [controller_client, controller_server] = std::move(controller_endpoints_result).value();

  fidl::WireResult<fuchsia_driver_framework::Node::AddChild> add_child_result =
      fidl::WireCall(node())->AddChild(std::move(args), std::move(controller_server), {});
  if (!add_child_result.ok()) {
    FDF_LOG(ERROR, "Failed to call FIDL AddChild: %s", add_child_result.status_string());
    return zx::error(add_child_result.status());
  }
  if (add_child_result->is_error()) {
    fuchsia_driver_framework::NodeError error = add_child_result->error_value();
    FDF_LOG(ERROR, "Failed to AddChild: %" PRIu32, static_cast<uint32_t>(error));
    return zx::error(ZX_ERR_INTERNAL);
  }

  return zx::ok(std::move(controller_client));
}

zx::result<> AmlCanvasDriver::Start() {
  zx::result<std::unique_ptr<inspect::ComponentInspector>> inspector_result =
      CreateComponentInspector();
  if (inspector_result.is_error()) {
    FDF_LOG(ERROR, "Failed to create component inspector: %s", inspector_result.status_string());
    return inspector_result.take_error();
  }
  component_inspector_ = std::move(inspector_result).value();

  zx::result<> compat_server_init_result =
      compat_server_.Initialize(incoming(), outgoing(), node_name(), name());
  if (compat_server_init_result.is_error()) {
    return compat_server_init_result.take_error();
  }

  auto canvas_result = CreateAndServeCanvas(component_inspector_->inspector());
  if (canvas_result.is_error()) {
    FDF_LOG(ERROR, "Failed to create AmlCanvas and set up service: %s",
            canvas_result.status_string());
    return canvas_result.take_error();
  }
  canvas_ = std::move(canvas_result).value();

  zx::result<fidl::ClientEnd<fuchsia_driver_framework::NodeController>> controller_client_result =
      AddChildNode(&compat_server_);
  if (controller_client_result.is_error()) {
    FDF_LOG(ERROR, "Failed to add child node: %s", controller_client_result.status_string());
    return controller_client_result.take_error();
  }
  controller_ = fidl::WireSyncClient(std::move(controller_client_result).value());

  return zx::ok();
}

}  // namespace aml_canvas

FUCHSIA_DRIVER_EXPORT(aml_canvas::AmlCanvasDriver);
