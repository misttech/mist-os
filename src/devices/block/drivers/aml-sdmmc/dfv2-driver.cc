// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "dfv2-driver.h"

#include <fidl/fuchsia.hardware.gpio/cpp/wire.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/metadata.h>
#include <lib/mmio/mmio.h>
#include <lib/zx/interrupt.h>

#include <string>

#include <bind/fuchsia/hardware/sdmmc/cpp/bind.h>

namespace aml_sdmmc {
namespace {

zx::result<aml_sdmmc_config_t> ParseMetadata(
    const fidl::VectorView<::fuchsia_driver_compat::wire::Metadata>& metadata_vector,
    fdf::Logger& logger) {
  for (const auto& metadata : metadata_vector) {
    if (metadata.type == DEVICE_METADATA_PRIVATE) {
      size_t size;
      auto status = metadata.data.get_prop_content_size(&size);
      if (status != ZX_OK) {
        FDF_LOGL(ERROR, logger, "Failed to get_prop_content_size: %s",
                 zx_status_get_string(status));
        return zx::error(status);
      }

      if (size != sizeof(aml_sdmmc_config_t)) {
        continue;
      }

      aml_sdmmc_config_t aml_sdmmc_config;
      status = metadata.data.read(&aml_sdmmc_config, 0, sizeof(aml_sdmmc_config_t));
      if (status != ZX_OK) {
        FDF_LOGL(ERROR, logger, "Failed to read metadata: %s", zx_status_get_string(status));
        return zx::error(status);
      }
      return zx::ok(aml_sdmmc_config);
    }
  }
  return zx::error(ZX_ERR_NOT_FOUND);
}

zx::result<pdev_device_info_t> FidlToBanjoDeviceInfo(
    const fuchsia_hardware_platform_device::wire::DeviceInfo& wire_dev_info) {
  pdev_device_info_t banjo_dev_info = {};
  if (wire_dev_info.has_vid()) {
    banjo_dev_info.vid = wire_dev_info.vid();
  }
  if (wire_dev_info.has_pid()) {
    banjo_dev_info.pid = wire_dev_info.pid();
  }
  if (wire_dev_info.has_did()) {
    banjo_dev_info.did = wire_dev_info.did();
  }
  if (wire_dev_info.has_mmio_count()) {
    banjo_dev_info.mmio_count = wire_dev_info.mmio_count();
  }
  if (wire_dev_info.has_irq_count()) {
    banjo_dev_info.irq_count = wire_dev_info.irq_count();
  }
  if (wire_dev_info.has_bti_count()) {
    banjo_dev_info.bti_count = wire_dev_info.bti_count();
  }
  if (wire_dev_info.has_smc_count()) {
    banjo_dev_info.smc_count = wire_dev_info.smc_count();
  }
  if (wire_dev_info.has_metadata_count()) {
    banjo_dev_info.metadata_count = wire_dev_info.metadata_count();
  }
  if (wire_dev_info.has_name()) {
    std::string name = std::string(wire_dev_info.name().get());
    if (name.size() > sizeof(banjo_dev_info.name)) {
      return zx::error(ZX_ERR_BUFFER_TOO_SMALL);
    }
    strncpy(banjo_dev_info.name, name.c_str(), sizeof(banjo_dev_info.name));
  }
  return zx::ok(banjo_dev_info);
}

}  // namespace

void Dfv2Driver::Start(fdf::StartCompleter completer) {
  parent_.Bind(std::move(node()));
  start_completer_.emplace(std::move(completer));
  compat_server_.OnInitialized(fit::bind_member<&Dfv2Driver::CompatServerInitialized>(this));
}

void Dfv2Driver::CompatServerInitialized(zx::result<> compat_result) {
  if (compat_result.is_error()) {
    return CompleteStart(compat_result.take_error());
  }

  aml_sdmmc_config_t config;
  {
    zx::result result = incoming()->Connect<fuchsia_driver_compat::Service::Device>();
    if (result.is_error() || !result->is_valid()) {
      FDF_LOGL(ERROR, logger(), "Failed to connect to compat service: %s", result.status_string());
      return CompleteStart(result.take_error());
    }
    auto parent_compat = fidl::WireSyncClient(std::move(result.value()));

    fidl::WireResult metadata = parent_compat->GetMetadata();
    if (!metadata.ok()) {
      FDF_LOGL(ERROR, logger(), "Call to GetMetadata failed: %s", metadata.status_string());
      return CompleteStart(zx::error(metadata.status()));
    }
    if (metadata->is_error()) {
      FDF_LOGL(ERROR, logger(), "Failed to GetMetadata: %s",
               zx_status_get_string(metadata->error_value()));
      return CompleteStart(metadata->take_error());
    }

    zx::result parsed_metadata = ParseMetadata(metadata.value()->metadata, logger());
    if (parsed_metadata.is_error()) {
      FDF_LOGL(ERROR, logger(), "Failed to ParseMetadata: %s",
               zx_status_get_string(parsed_metadata.error_value()));
      return CompleteStart(parsed_metadata.take_error());
    }
    config = parsed_metadata.value();
  }

  {
    zx::result pdev = incoming()->Connect<fuchsia_hardware_platform_device::Service::Device>();
    if (pdev.is_error() || !pdev->is_valid()) {
      FDF_LOGL(ERROR, logger(), "Failed to connect to platform device: %s", pdev.status_string());
      return CompleteStart(pdev.take_error());
    }

    if (zx::result status = InitResources(std::move(pdev.value()), config); status.is_error()) {
      return CompleteStart(status.take_error());
    }
  }

  auto inspect_sink = incoming()->Connect<fuchsia_inspect::InspectSink>();
  if (inspect_sink.is_error() || !inspect_sink->is_valid()) {
    FDF_LOGL(ERROR, logger(), "Failed to connect to inspect sink: %s",
             inspect_sink.status_string());
    return CompleteStart(inspect_sink.take_error());
  }
  exposed_inspector_.emplace(inspect::ComponentInspector(
      dispatcher(), {.inspector = inspector(), .client_end = std::move(inspect_sink.value())}));

  {
    fuchsia_hardware_sdmmc::SdmmcService::InstanceHandler handler({
        .sdmmc = fit::bind_member<&Dfv2Driver::Serve>(this),
    });
    auto result = outgoing()->AddService<fuchsia_hardware_sdmmc::SdmmcService>(std::move(handler));
    if (result.is_error()) {
      FDF_LOGL(ERROR, logger(), "Failed to add service: %s", result.status_string());
      return CompleteStart(result.take_error());
    }
  }

  zx::result controller_endpoints =
      fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
  if (!controller_endpoints.is_ok()) {
    FDF_LOGL(ERROR, logger(), "Failed to create controller endpoints: %s",
             controller_endpoints.status_string());
    return CompleteStart(controller_endpoints.take_error());
  }

  controller_.Bind(std::move(controller_endpoints->client));

  fidl::Arena arena;

  fidl::VectorView<fuchsia_driver_framework::wire::NodeProperty> properties(arena, 2);
  properties[0] = fdf::MakeProperty(arena, BIND_PROTOCOL, ZX_PROTOCOL_SDMMC);
  properties[1] = fdf::MakeProperty(arena, bind_fuchsia_hardware_sdmmc::SDMMCSERVICE,
                                    bind_fuchsia_hardware_sdmmc::SDMMCSERVICE_DRIVERTRANSPORT);

  std::vector<fuchsia_component_decl::wire::Offer> offers = compat_server_.CreateOffers(arena);
  offers.push_back(
      fdf::MakeOffer<fuchsia_hardware_sdmmc::SdmmcService>(arena, component::kDefaultInstance));

  const auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena)
                        .name(arena, name())
                        .offers(arena, std::move(offers))
                        .properties(properties)
                        .Build();

  auto result = parent_->AddChild(args, std::move(controller_endpoints->server), {});
  if (!result.ok()) {
    FDF_LOGL(ERROR, logger(), "Failed to add child: %s", result.status_string());
    return CompleteStart(zx::error(result.status()));
  }

  FDF_LOGL(INFO, logger(), "Completed start hook");

  return CompleteStart(zx::ok());
}

zx::result<> Dfv2Driver::InitResources(
    fidl::ClientEnd<fuchsia_hardware_platform_device::Device> pdev_client,
    aml_sdmmc_config_t config) {
  fidl::WireSyncClient pdev(std::move(pdev_client));

  std::optional<fdf::MmioBuffer> mmio;
  {
    const auto result = pdev->GetMmio(0);
    if (!result.ok()) {
      FDF_LOGL(ERROR, logger(), "Call to get MMIO failed: %s", result.status_string());
      return zx::error(result.status());
    }
    if (!result->is_ok()) {
      FDF_LOGL(ERROR, logger(), "Failed to get MMIO: %s",
               zx_status_get_string(result->error_value()));
      return zx::error(result->error_value());
    }

    const auto& mmio_params = result->value();
    if (!mmio_params->has_offset() || !mmio_params->has_size() || !mmio_params->has_vmo()) {
      FDF_LOGL(ERROR, logger(), "Platform device provided invalid MMIO");
      return zx::error(ZX_ERR_BAD_STATE);
    };

    auto mmio_result =
        fdf::MmioBuffer::Create(mmio_params->offset(), mmio_params->size(),
                                std::move(mmio_params->vmo()), ZX_CACHE_POLICY_UNCACHED_DEVICE);
    if (mmio_result.is_error()) {
      FDF_LOGL(ERROR, logger(), "Failed to map MMIO: %s", mmio_result.status_string());
      return mmio_result.take_error();
    }
    mmio = std::move(mmio_result.value());
  }

  zx::interrupt irq;
  {
    const auto result = pdev->GetInterrupt(0, 0);
    if (!result.ok()) {
      FDF_LOGL(ERROR, logger(), "Call to get interrupt failed: %s", result.status_string());
      return zx::error(result.status());
    }
    if (!result->is_ok()) {
      FDF_LOGL(ERROR, logger(), "Failed to get interrupt: %s",
               zx_status_get_string(result->error_value()));
      return zx::error(result->error_value());
    }
    irq = std::move(result->value()->irq);
  }

  zx::bti bti;
  {
    const auto result = pdev->GetBti(0);
    if (!result.ok()) {
      FDF_LOGL(ERROR, logger(), "Call to get BTI failed: %s", result.status_string());
      return zx::error(result.status());
    }
    if (!result->is_ok()) {
      FDF_LOGL(ERROR, logger(), "Failed to get BTI: %s",
               zx_status_get_string(result->error_value()));
      return zx::error(result->error_value());
    }
    bti = std::move(result->value()->bti);
  }

  // Optional protocol.
  fidl::ClientEnd<fuchsia_hardware_gpio::Gpio> reset_gpio;
  const char* kGpioWifiFragmentNames[2] = {"gpio-wifi-power-on", "gpio"};
  for (const char* fragment : kGpioWifiFragmentNames) {
    zx::result result = incoming()->Connect<fuchsia_hardware_gpio::Service::Device>(fragment);
    if (!result.is_error() && result->is_valid()) {
      auto gpio = fidl::WireSyncClient<fuchsia_hardware_gpio::Gpio>(std::move(result.value()));
      if (gpio->GetName().ok()) {
        reset_gpio = gpio.TakeClientEnd();
        break;
      }
    }
  }

  auto buffer_factory = dma_buffer::CreateBufferFactory();
  std::unique_ptr<dma_buffer::ContiguousBuffer> descs_buffer;
  zx_status_t status = buffer_factory->CreateContiguous(
      bti, kMaxDmaDescriptors * sizeof(aml_sdmmc_desc_t), 0, &descs_buffer);
  if (status != ZX_OK) {
    FDF_LOGL(ERROR, logger(), "Failed to allocate dma descriptors");
    return zx::error(status);
  }

  fidl::ClientEnd<fuchsia_hardware_clock::Clock> clock_gate;
  zx::result result = incoming()->Connect<fuchsia_hardware_clock::Service::Clock>("clock-gate");
  if (result.is_ok() && result->is_valid()) {
    auto clock = fidl::WireSyncClient<fuchsia_hardware_clock::Clock>(std::move(result.value()));
    const fidl::WireResult result = clock->Enable();
    if (result.ok()) {
      if (result->is_error()) {
        FDF_LOGL(ERROR, logger(), "Failed to enable clock: %s",
                 zx_status_get_string(result->error_value()));
        return zx::error(result->error_value());
      }
      clock_gate = clock.TakeClientEnd();
    }
  }

  SetUpResources(std::move(bti), std::move(*mmio), config, std::move(irq), std::move(reset_gpio),
                 std::move(descs_buffer), std::move(clock_gate));

  {
    const auto result = pdev->GetDeviceInfo();
    if (!result.ok()) {
      FDF_LOGL(ERROR, logger(), "Call to get device info failed: %s", result.status_string());
      return zx::error(result.status());
    }
    if (!result->is_ok()) {
      FDF_LOGL(ERROR, logger(), "Failed to get device info: %s",
               zx_status_get_string(result->error_value()));
      return zx::error(result->error_value());
    }

    zx::result dev_info = FidlToBanjoDeviceInfo(*result->value());
    if (dev_info.is_error()) {
      return dev_info.take_error();
    }

    status = Init(dev_info.value());
    if (status != ZX_OK) {
      return zx::error(status);
    }
  }

  return zx::success();
}

void Dfv2Driver::Serve(fdf::ServerEnd<fuchsia_hardware_sdmmc::Sdmmc> request) {
  fdf::BindServer(driver_dispatcher()->get(), std::move(request), this);
}

void Dfv2Driver::CompleteStart(zx::result<> result) {
  ZX_ASSERT(start_completer_.has_value());
  start_completer_.value()(result);
  start_completer_.reset();
}

}  // namespace aml_sdmmc
