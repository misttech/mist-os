// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/drivers/aml-g12-tdm/composite.h"

#include <fidl/fuchsia.driver.compat/cpp/wire.h>
#include <lib/ddk/platform-defs.h>
#include <lib/driver/component/cpp/driver_export.h>

namespace audio::aml_g12 {

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
  auto controller_endpoints = fidl::Endpoints<fuchsia_driver_framework::NodeController>::Create();

  zx::result node_endpoints = fidl::CreateEndpoints<fuchsia_driver_framework::Node>();
  ZX_ASSERT_MSG(node_endpoints.is_ok(), "Node end point creation failed: %s",
                node_endpoints.status_string());

  fidl::WireResult result = fidl::WireCall(node())->AddChild(
      args, std::move(controller_endpoints.server), std::move(node_endpoints->server));
  if (!result.ok()) {
    FDF_SLOG(ERROR, "Call to add child failed", KV("status", result.status_string()));
    return zx::error(result.status());
  }
  if (!result->is_ok()) {
    FDF_SLOG(ERROR, "Failed to add child", KV("error", result.FormatDescription().c_str()));
    return zx::error(ZX_ERR_INTERNAL);
  }
  controller_.Bind(std::move(controller_endpoints.client));
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
  // TODO(https://fxbug.dev/42082341): If we change the engines underlying AmlTdmDevice objects such
  // that they take an MmioView, then we can get only one MmioBuffer here, own it in this driver and
  // pass MmioViews to the underlying AmlTdmDevice objects.
  std::array<std::optional<fdf::MmioBuffer>, kNumberOfTdmEngines> mmios;
  for (size_t i = 0; i < kNumberOfTdmEngines; ++i) {
    // There is one MMIO region with index 0 used by this driver.
    auto get_mmio_result = pdev_->GetMmioById(0);
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
  auto get_bti_result = pdev_->GetBtiById(0);
  if (!get_bti_result.ok()) {
    FDF_LOG(ERROR, "Call to get BTI failed: %s", get_bti_result.status_string());
    return zx::error(get_bti_result.status());
  }
  if (!get_bti_result->is_ok()) {
    FDF_LOG(ERROR, "Platform device returned error for get BTI: %s",
            zx_status_get_string(get_bti_result->error_value()));
    return zx::error(get_bti_result->error_value());
  }

  zx::result clock_gate_result =
      incoming()->Connect<fuchsia_hardware_clock::Service::Clock>("clock-gate");
  if (clock_gate_result.is_error() || !clock_gate_result->is_valid()) {
    FDF_LOG(ERROR, "Connect to clock-gate failed: %s", clock_gate_result.status_string());
    return zx::error(clock_gate_result.error_value());
  }
  fidl::WireSyncClient<fuchsia_hardware_clock::Clock> gate_client(
      std::move(clock_gate_result.value()));

  zx::result clock_pll_result =
      incoming()->Connect<fuchsia_hardware_clock::Service::Clock>("clock-pll");
  if (clock_pll_result.is_error() || !clock_pll_result->is_valid()) {
    FDF_LOG(ERROR, "Connect to clock-pll failed: %s", clock_pll_result.status_string());
    return zx::error(clock_pll_result.error_value());
  }
  fidl::WireSyncClient<fuchsia_hardware_clock::Clock> pll_client(
      std::move(clock_pll_result.value()));

  std::array<const char*, kNumberOfPipelines> sclk_gpio_names = {
      "gpio-tdm-a-sclk",
      "gpio-tdm-b-sclk",
      "gpio-tdm-c-sclk",
  };
  std::vector<fidl::WireSyncClient<fuchsia_hardware_gpio::Gpio>> gpio_sclk_clients;
  for (auto& sclk_gpio_name : sclk_gpio_names) {
    zx::result gpio_result =
        incoming()->Connect<fuchsia_hardware_gpio::Service::Device>(sclk_gpio_name);
    if (gpio_result.is_error() || !gpio_result->is_valid()) {
      FDF_LOG(ERROR, "Connect to GPIO %s failed: %s", sclk_gpio_name, gpio_result.status_string());
      return zx::error(gpio_result.error_value());
    }
    fidl::WireSyncClient<fuchsia_hardware_gpio::Gpio> gpio_sclk_client(
        std::move(gpio_result.value()));
    // Only save the GPIO client if we can communicate with it (we use a method with no side
    // effects) since optional nodes are valid even if they are not configured in the board driver.
    auto gpio_name_result = gpio_sclk_client->GetName();
    if (gpio_name_result.ok()) {
      gpio_sclk_clients.emplace_back(std::move(gpio_sclk_client));
    }
  }

  auto device_info_result = pdev_->GetNodeDeviceInfo();
  if (!device_info_result.ok()) {
    FDF_LOG(ERROR, "Call to get node device info failed: %s", device_info_result.status_string());
    return zx::error(device_info_result.status());
  }
  if (!device_info_result->is_ok()) {
    FDF_LOG(ERROR, "Failed to get node device info: %s",
            zx_status_get_string(device_info_result->error_value()));
    return zx::error(device_info_result->error_value());
  }

  if ((*device_info_result)->vid() == PDEV_VID_GENERIC &&
      (*device_info_result)->pid() == PDEV_PID_GENERIC &&
      (*device_info_result)->did() == PDEV_DID_DEVICETREE_NODE) {
    // TODO(https://fxbug.dev/318736574) : Remove and rely only on GetDeviceInfo.
    auto board_info_result = pdev_->GetBoardInfo();
    if (!board_info_result.ok()) {
      FDF_LOG(ERROR, "GetBoardInfo failed: %s",
              zx_status_get_string(board_info_result->error_value()));
      return zx::error(board_info_result->error_value());
    }

    if ((*board_info_result)->vid() == PDEV_VID_KHADAS) {
      switch ((*board_info_result)->pid()) {
        case PDEV_PID_VIM3:
          (*device_info_result)->pid() = PDEV_PID_AMLOGIC_A311D;
          break;
        default:
          FDF_LOG(ERROR, "Unsupported PID 0x%x for VID 0x%x", (*board_info_result)->pid(),
                  (*board_info_result)->vid());
          return zx::error(ZX_ERR_NOT_SUPPORTED);
      }
    } else {
      FDF_LOG(ERROR, "Unsupported VID 0x%x", (*board_info_result)->vid());
      return zx::error(ZX_ERR_NOT_SUPPORTED);
    }
  }

  metadata::AmlVersion aml_version = {};
  switch ((*device_info_result)->pid()) {
    case PDEV_PID_AMLOGIC_A311D:
      aml_version = metadata::AmlVersion::kA311D;
      break;
    case PDEV_PID_AMLOGIC_T931:
      [[fallthrough]];
    case PDEV_PID_AMLOGIC_S905D2:
      aml_version = metadata::AmlVersion::kS905D2G;  // Also works with T931G.
      break;
    case PDEV_PID_AMLOGIC_S905D3:
      aml_version = metadata::AmlVersion::kS905D3G;
      break;
    case PDEV_PID_AMLOGIC_A5:
      aml_version = metadata::AmlVersion::kA5;
      break;
    case PDEV_PID_AMLOGIC_A1:
      aml_version = metadata::AmlVersion::kA1;
      break;
    default:
      FDF_LOG(ERROR, "Unsupported PID 0x%X", (*device_info_result)->pid());
      return zx::error(ZX_ERR_NOT_SUPPORTED);
  }

  server_ = std::make_unique<AudioCompositeServer>(
      std::move(mmios), std::move((*get_bti_result)->bti), dispatcher(), aml_version,
      std::move(gate_client), std::move(pll_client), std::move(gpio_sclk_clients));

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
