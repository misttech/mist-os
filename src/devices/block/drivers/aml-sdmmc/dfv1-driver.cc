// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "dfv1-driver.h"

#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/metadata.h>
#include <lib/device-protocol/pdev-fidl.h>

namespace aml_sdmmc {

void DriverLogTrace(const char* message) { zxlogf(TRACE, "%s", message); }

void DriverLogInfo(const char* message) { zxlogf(INFO, "%s", message); }

void DriverLogWarning(const char* message) { zxlogf(WARNING, "%s", message); }

void DriverLogError(const char* message) { zxlogf(ERROR, "%s", message); }

zx_status_t Dfv1Driver::Bind() {
  auto protocol = [this](fdf::ServerEnd<fuchsia_hardware_sdmmc::Sdmmc> server_end) mutable {
    fdf::BindServer(dispatcher_, std::move(server_end), this);
  };
  fuchsia_hardware_sdmmc::SdmmcService::InstanceHandler handler({.sdmmc = std::move(protocol)});
  auto result = outgoing_dir_.AddService<fuchsia_hardware_sdmmc::SdmmcService>(std::move(handler));
  if (result.is_error()) {
    zxlogf(ERROR, "Failed to add service to outgoing directory: %s", result.status_string());
    return result.status_value();
  }
  auto endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
  if (endpoints.is_error()) {
    return endpoints.status_value();
  }
  result = outgoing_dir_.Serve(std::move(endpoints->server));
  if (result.is_error()) {
    zxlogf(ERROR, "Failed to serve outgoing directory: %s", result.status_string());
    return result.status_value();
  }

  // TODO(fxbug.dev/134787): Offer SdmmcService::Name to use FIDL with the sdmmc core block driver.
  std::array<const char*, 0> offers;  // = { fuchsia_hardware_sdmmc::SdmmcService::Name, };

  // Note: This name can't be changed without migrating users in other repos.
  zx_status_t status = DdkAdd(ddk::DeviceAddArgs("aml-sd-emmc")
                                  .set_inspect_vmo(GetInspectVmo())
                                  .forward_metadata(parent(), DEVICE_METADATA_SDMMC)
                                  .forward_metadata(parent(), DEVICE_METADATA_GPT_INFO)
                                  .set_outgoing_dir(endpoints->client.TakeChannel())
                                  .set_runtime_service_offers(offers));
  if (status != ZX_OK) {
    zxlogf(ERROR, "DdkAdd failed");
  }
  return status;
}

zx_status_t Dfv1Driver::Create(void* ctx, zx_device_t* parent) {
  auto pdev = ddk::PDevFidl::FromFragment(parent);
  if (!pdev.is_valid()) {
    zxlogf(ERROR, "Could not get pdev.");
    return ZX_ERR_NO_RESOURCES;
  }

  zx::bti bti;
  zx_status_t status = ZX_OK;
  if ((status = pdev.GetBti(0, &bti)) != ZX_OK) {
    zxlogf(ERROR, "Failed to get BTI: %d", status);
    return status;
  }

  std::optional<fdf::MmioBuffer> mmio;
  status = pdev.MapMmio(0, &mmio);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to get mmio: %d", status);
    return status;
  }

  // Populate board specific information
  aml_sdmmc_config_t config;
  size_t actual;
  status = device_get_metadata(parent, DEVICE_METADATA_PRIVATE, &config, sizeof(config), &actual);
  if (status != ZX_OK || actual != sizeof(config)) {
    zxlogf(ERROR, "Failed to get metadata: %d", status);
    return status;
  }

  zx::interrupt irq;
  if ((status = pdev.GetInterrupt(0, &irq)) != ZX_OK) {
    zxlogf(ERROR, "Failed to get interrupt: %d", status);
    return status;
  }

  // Optional protocol.
  fidl::ClientEnd<fuchsia_hardware_gpio::Gpio> reset_gpio;
  const char* kGpioWifiFragmentNames[2] = {"gpio-wifi-power-on", "gpio"};
  for (const char* fragment : kGpioWifiFragmentNames) {
    zx::result reset_gpio_result =
        DdkConnectFragmentFidlProtocol<fuchsia_hardware_gpio::Service::Device>(parent, fragment);
    if (reset_gpio_result.is_ok()) {
      reset_gpio = std::move(reset_gpio_result.value());
      break;
    }
    if (reset_gpio_result.status_value() != ZX_ERR_NOT_FOUND) {
      zxlogf(ERROR, "Failed to get gpio protocol from fragment %s: %s", fragment,
             reset_gpio_result.status_string());
      return reset_gpio_result.status_value();
    }
  }

  aml_sdmmc::IoBuffer descs_buffer;
  status = descs_buffer.Init(bti.get(), kMaxDmaDescriptors * sizeof(aml_sdmmc_desc_t),
                             IO_BUFFER_RW | IO_BUFFER_CONTIG);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to allocate dma descriptors");
    return status;
  }

  auto dev = std::make_unique<Dfv1Driver>(parent);
  dev->SetUpResources(std::move(bti), std::move(*mmio), config, std::move(irq),
                      std::move(reset_gpio), std::move(descs_buffer));

  pdev_device_info_t dev_info;
  if ((status = pdev.GetDeviceInfo(&dev_info)) != ZX_OK) {
    zxlogf(ERROR, "Failed to get device info: %d", status);
    return status;
  }

  if ((status = dev->Init(dev_info)) != ZX_OK) {
    return status;
  }

  if ((status = dev->Bind()) != ZX_OK) {
    return status;
  }

  // devmgr is now in charge of the device.
  [[maybe_unused]] auto* placeholder = dev.release();
  return ZX_OK;
}

void Dfv1Driver::DdkSuspend(ddk::SuspendTxn txn) {
  ShutDown();
  txn.Reply(ZX_OK, txn.requested_state());
}

void Dfv1Driver::DdkRelease() {
  ShutDown();
  delete this;
}

static constexpr zx_driver_ops_t aml_sdmmc_driver_ops = []() {
  zx_driver_ops_t driver_ops = {};
  driver_ops.version = DRIVER_OPS_VERSION;
  driver_ops.bind = Dfv1Driver::Create;
  return driver_ops;
}();

}  // namespace aml_sdmmc

ZIRCON_DRIVER(aml_sdmmc, aml_sdmmc::aml_sdmmc_driver_ops, "zircon", "0.1");
