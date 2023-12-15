// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/drivers/aml-g12-tdm/composite.h"

#include <fidl/fuchsia.driver.compat/cpp/wire.h>
#include <lib/driver/component/cpp/driver_export.h>

namespace audio::aml_g12 {

// TODO(b/300991607): Use clock-gate, clock-pll, and gpio-init services.

zx::result<> Driver::CreateDevfsNode() {
  fidl::Arena arena;
  zx::result connector = devfs_connector_.Bind(server_->dispatcher());
  if (connector.is_error()) {
    return connector.take_error();
  }

  auto devfs = fuchsia_driver_framework::wire::DevfsAddArgs::Builder(arena)
                   .connector(std::move(connector.value()))
                   .class_name("audio-composite");

  auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena)
                  .name(arena, kDriverName)
                  .devfs_args(devfs.Build())
                  .Build();

  // Create endpoints of the `NodeController` for the node.
  zx::result controller_endpoints =
      fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
  ZX_ASSERT_MSG(controller_endpoints.is_ok(), "Controller end point creation failed: %s",
                controller_endpoints.status_string());

  zx::result node_endpoints = fidl::CreateEndpoints<fuchsia_driver_framework::Node>();
  ZX_ASSERT_MSG(node_endpoints.is_ok(), "Node end point creation failed: %s",
                node_endpoints.status_string());

  fidl::WireResult result = fidl::WireCall(node())->AddChild(
      args, std::move(controller_endpoints->server), std::move(node_endpoints->server));
  if (!result.ok()) {
    FDF_SLOG(ERROR, "Call to add child failed", KV("status", result.status_string()));
    return zx::error(result.status());
  }
  if (!result->is_ok()) {
    FDF_SLOG(ERROR, "Failed to add child", KV("error", result.FormatDescription().c_str()));
    return zx::error(ZX_ERR_INTERNAL);
  }
  controller_.Bind(std::move(controller_endpoints->client));
  node_.Bind(std::move(node_endpoints->client));

  return zx::ok();
}

zx::result<> Driver::Start() {
  zx::result pdev = incoming()->Connect<fuchsia_hardware_platform_device::Service::Device>();
  if (pdev.is_error() || !pdev->is_valid()) {
    FDF_LOG(ERROR, "Failed to connect to platform device: %s", pdev.status_string());
    return pdev.take_error();
  }
  pdev_.Bind(std::move(pdev.value()));
  // We get one MMIO per engine.
  // TODO(fxbug.dev/132252): If we change the engines underlying AmlTdmDevice objects such that
  // they take an MmioView, then we can get only one MmioBuffer here, own it in this driver and
  // pass MmioViews to the underlying AmlTdmDevice objects.
  std::array<std::optional<fdf::MmioBuffer>, kNumberOfTdmEngines> mmios;
  for (size_t i = 0; i < kNumberOfTdmEngines; ++i) {
    // There is one MMIO region with index 0 used by this driver.
    auto get_mmio_result = pdev_->GetMmio(0);
    if (!get_mmio_result.ok()) {
      FDF_LOG(ERROR, "Call to get MMIO failed: %s", get_mmio_result.status_string());
      return zx::error(get_mmio_result.status());
    }
    if (!get_mmio_result->is_ok()) {
      FDF_LOG(ERROR, "Platform device returned error for get MMIO: %s",
              zx_status_get_string(get_mmio_result->error_value()));
      return zx::error(get_mmio_result->error_value());
    }

    const auto& mmio_params = get_mmio_result->value();
    if (!mmio_params->has_offset() || !mmio_params->has_size() || !mmio_params->has_vmo()) {
      FDF_LOG(ERROR, "Platform device provided invalid MMIO");
      return zx::error(ZX_ERR_BAD_STATE);
    };

    auto mmio =
        fdf::MmioBuffer::Create(mmio_params->offset(), mmio_params->size(),
                                std::move(mmio_params->vmo()), ZX_CACHE_POLICY_UNCACHED_DEVICE);
    if (mmio.is_error()) {
      FDF_LOG(ERROR, "Failed to map MMIO: %s", mmio.status_string());
      return zx::error(mmio.error_value());
    }
    mmios[i] = std::make_optional(std::move(*mmio));
  }

  // There is one BTI with index 0 used by this driver.
  auto get_bti_result = pdev_->GetBti(0);
  if (!get_bti_result.ok()) {
    FDF_LOG(ERROR, "Call to get BTI failed: %s", get_bti_result.status_string());
    return zx::error(get_bti_result.status());
  }
  if (!get_bti_result->is_ok()) {
    FDF_LOG(ERROR, "Platform device returned error for get BTI: %s",
            zx_status_get_string(get_bti_result->error_value()));
    return zx::error(get_bti_result->error_value());
  }
  server_ = std::make_unique<AudioCompositeServer>(std::move(mmios),
                                                   std::move((*get_bti_result)->bti), dispatcher());

  auto result = outgoing()->component().AddUnmanagedProtocol<fuchsia_hardware_audio::Composite>(
      bindings_.CreateHandler(server_.get(), dispatcher(), fidl::kIgnoreBindingClosure),
      kDriverName);
  if (result.is_error()) {
    FDF_LOG(ERROR, "Failed to add Device service %s", result.status_string());
    return result.take_error();
  }

  if (zx::result result = CreateDevfsNode(); result.is_error()) {
    FDF_LOG(ERROR, "Failed to export to devfs %s", result.status_string());
    return result.take_error();
  }

  FDF_SLOG(INFO, "Started");

  return zx::ok();
}

}  // namespace audio::aml_g12

FUCHSIA_DRIVER_EXPORT(audio::aml_g12::Driver);
