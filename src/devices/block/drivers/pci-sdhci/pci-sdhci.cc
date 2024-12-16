// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "pci-sdhci.h"

#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/device-protocol/pci.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/param.h>
#include <threads.h>
#include <unistd.h>
#include <zircon/status.h>

#define HOST_CONTROL1_OFFSET 0x28
#define SDHCI_EMMC_HW_RESET (1 << 12)

constexpr auto kTag = "pci-sdhci";

namespace sdhci {

PciSdhci::PciSdhci(zx_device_t* parent) : DeviceType(parent) {}

void PciSdhci::GetInterrupt(fdf::Arena& arena, GetInterruptCompleter::Sync& completer) {
  // select irq mode
  fuchsia_hardware_pci::InterruptMode mode = fuchsia_hardware_pci::InterruptMode::kDisabled;
  zx_status_t status = pci_.ConfigureInterruptMode(1, &mode);
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: error setting irq mode: %s", kTag, zx_status_get_string(status));
    completer.buffer(arena).ReplyError(status);
    return;
  }

  // get irq handle
  zx::interrupt interrupt;
  status = pci_.MapInterrupt(0, &interrupt);
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: error getting irq handle: %s", kTag, zx_status_get_string(status));
    completer.buffer(arena).ReplyError(status);
    return;
  }

  completer.buffer(arena).ReplySuccess(std::move(interrupt));
}

void PciSdhci::GetMmio(fdf::Arena& arena, GetMmioCompleter::Sync& completer) {
  if (!mmio_.has_value()) {
    zx_status_t status = pci_.MapMmio(0u, ZX_CACHE_POLICY_UNCACHED_DEVICE, &mmio_);
    if (status != ZX_OK) {
      zxlogf(ERROR, "%s: error mapping register window: %s", kTag, zx_status_get_string(status));
      completer.buffer(arena).ReplyError(status);
      return;
    }
  }
  auto offset = mmio_->get_offset();
  zx::vmo vmo;
  zx_status_t status = mmio_->get_vmo()->duplicate(ZX_RIGHT_SAME_RIGHTS, &vmo);
  if (status != ZX_OK) {
    completer.buffer(arena).ReplyError(status);
    return;
  }

  completer.buffer(arena).ReplySuccess(std::move(vmo), offset);
}

void PciSdhci::GetBti(GetBtiRequestView request, fdf::Arena& arena,
                      GetBtiCompleter::Sync& completer) {
  if (!bti_.is_valid()) {
    zx_status_t status = pci_.GetBti(request->index, &bti_);
    if (status != ZX_OK) {
      completer.buffer(arena).ReplyError(status);
      return;
    }
  }

  zx::bti bti;
  zx_status_t status = bti_.duplicate(ZX_RIGHT_SAME_RIGHTS, &bti);
  if (status != ZX_OK) {
    completer.buffer(arena).ReplyError(status);
    return;
  }

  completer.buffer(arena).ReplySuccess(std::move(bti));
}

void PciSdhci::GetBaseClock(fdf::Arena& arena, GetBaseClockCompleter::Sync& completer) {
  completer.buffer(arena).Reply(0);
}

void PciSdhci::GetQuirks(fdf::Arena& arena, GetQuirksCompleter::Sync& completer) {
  completer.buffer(arena).Reply(fuchsia_hardware_sdhci::Quirk::kStripResponseCrcPreserveOrder, 0);
}

void PciSdhci::HwReset(fdf::Arena& arena, HwResetCompleter::Sync& completer) {
  if (!mmio_.has_value()) {
    completer.buffer(arena).Reply();
    return;
  }
  uint32_t val = mmio_->Read32(HOST_CONTROL1_OFFSET);
  val |= SDHCI_EMMC_HW_RESET;
  mmio_->Write32(val, HOST_CONTROL1_OFFSET);
  // minimum is 1us but wait 9us for good measure
  zx_nanosleep(zx_deadline_after(ZX_USEC(9)));
  val &= ~SDHCI_EMMC_HW_RESET;
  mmio_->Write32(val, HOST_CONTROL1_OFFSET);
  // minimum is 200us but wait 300us for good measure
  zx_nanosleep(zx_deadline_after(ZX_USEC(300)));
  completer.buffer(arena).Reply();
}

void PciSdhci::VendorSetBusClock(VendorSetBusClockRequestView request, fdf::Arena& arena,
                                 VendorSetBusClockCompleter::Sync& completer) {
  completer.buffer(arena).ReplyError(ZX_ERR_STOP);
}

void PciSdhci::DdkUnbind(ddk::UnbindTxn txn) { device_unbind_reply(zxdev()); }

zx_status_t PciSdhci::Bind(void* /* unused */, zx_device_t* parent) {
  auto dev = std::make_unique<PciSdhci>(parent);
  if (!dev) {
    zxlogf(ERROR, "%s: out of memory", kTag);
    return ZX_ERR_NO_MEMORY;
  }

  zx_status_t status = dev->Init();
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to initialize device: %s", zx_status_get_string(status));
    return status;
  }

  // The object is owned by the DDK, now that it has been added. It will be deleted
  // when the device is released.
  [[maybe_unused]] auto ptr = dev.release();

  return ZX_OK;
}

zx_status_t PciSdhci::Init() {
  pci_ = ddk::Pci(parent_, "pci");
  if (!pci_.is_valid()) {
    zxlogf(ERROR, "Failed to connect to PCI protocol");
    return ZX_ERR_INTERNAL;
  }

  zx_status_t status = pci_.SetBusMastering(true);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to set bus mastering: %s", zx_status_get_string(status));
    return status;
  }

  {
    fuchsia_hardware_sdhci::Service::InstanceHandler handler(
        {.device = bindings_.CreateHandler(this, fdf::Dispatcher::GetCurrent()->get(),
                                           fidl::kIgnoreBindingClosure)});

    zx::result result = outgoing_.AddService<fuchsia_hardware_sdhci::Service>(std::move(handler));
    if (result.is_error()) {
      zxlogf(ERROR, "Failed to add sdhci fidl service to outgoing directory: %s",
             result.status_string());
      return result.error_value();
    }
  }

  auto endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
  if (endpoints.is_error()) {
    zxlogf(ERROR, "Failed to create endpoints: %s", endpoints.status_string());
    return endpoints.status_value();
  }

  zx::result result = outgoing_.Serve(std::move(endpoints->server));
  if (result.is_error()) {
    zxlogf(ERROR, "Failed to serve outgoing directory: %s", result.status_string());
    return result.error_value();
  }

  std::array offers = {
      fuchsia_hardware_sdhci::Service::Name,
  };

  status = DdkAdd(ddk::DeviceAddArgs("pci-sdhci")
                      .set_runtime_service_offers(offers)
                      .set_outgoing_dir(endpoints->client.TakeChannel()));
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to add device: %s", zx_status_get_string(status));
    return status;
  }

  return ZX_OK;
}

static zx_driver_ops_t pci_sdhci_driver_ops = {
    .version = DRIVER_OPS_VERSION,
    .bind = PciSdhci::Bind,
};

void PciSdhci::DdkRelease() { delete this; }

}  // namespace sdhci

ZIRCON_DRIVER(pci_sdhci, sdhci::pci_sdhci_driver_ops, "zircon", "0.1");
