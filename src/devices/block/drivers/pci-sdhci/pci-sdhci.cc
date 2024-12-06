// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "pci-sdhci.h"

#include <fuchsia/hardware/sdhci/cpp/banjo.h>
#include <inttypes.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/device-protocol/pci.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/param.h>
#include <threads.h>
#include <unistd.h>
#include <zircon/status.h>

#define HOST_CONTROL1_OFFSET 0x28
#define SDHCI_EMMC_HW_RESET (1 << 12)

constexpr auto kTag = "pci-sdhci";

namespace sdhci {

PciSdhci::PciSdhci(zx_device_t* parent) : DeviceType(parent) {}

zx_status_t PciSdhci::SdhciGetInterrupt(zx::interrupt* interrupt_out) {
  // select irq mode
  fuchsia_hardware_pci::InterruptMode mode = fuchsia_hardware_pci::InterruptMode::kDisabled;
  zx_status_t status = pci_.ConfigureInterruptMode(1, &mode);
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: error setting irq mode: %s", kTag, zx_status_get_string(status));
    return status;
  }

  // get irq handle
  status = pci_.MapInterrupt(0, interrupt_out);
  if (status != ZX_OK) {
    zxlogf(ERROR, "%s: error getting irq handle: %s", kTag, zx_status_get_string(status));
  }
  return status;
}

void PciSdhci::GetInterrupt(fdf::Arena& arena, GetInterruptCompleter::Sync& completer) {
  zx::interrupt irq;
  if (zx_status_t status = SdhciGetInterrupt(&irq); status != ZX_OK) {
    completer.buffer(arena).ReplyError(status);
    return;
  }
  completer.buffer(arena).ReplySuccess(std::move(irq));
}

zx_status_t PciSdhci::SdhciGetMmio(zx::vmo* out, zx_off_t* out_offset) {
  if (!mmio_.has_value()) {
    zx_status_t status = pci_.MapMmio(0u, ZX_CACHE_POLICY_UNCACHED_DEVICE, &mmio_);
    if (status != ZX_OK) {
      zxlogf(ERROR, "%s: error mapping register window: %s", kTag, zx_status_get_string(status));
      return status;
    }
  }
  *out_offset = mmio_->get_offset();
  return mmio_->get_vmo()->duplicate(ZX_RIGHT_SAME_RIGHTS, out);
}

void PciSdhci::GetMmio(fdf::Arena& arena, GetMmioCompleter::Sync& completer) {
  zx::vmo vmo;
  zx_off_t offset;
  if (zx_status_t status = SdhciGetMmio(&vmo, &offset); status != ZX_OK) {
    completer.buffer(arena).ReplyError(status);
    return;
  }
  completer.buffer(arena).ReplySuccess(std::move(vmo), offset);
}

zx_status_t PciSdhci::SdhciGetBti(uint32_t index, zx::bti* out_bti) {
  if (!bti_.is_valid()) {
    zx_status_t st = pci_.GetBti(index, &bti_);
    if (st != ZX_OK) {
      return st;
    }
  }
  return bti_.duplicate(ZX_RIGHT_SAME_RIGHTS, out_bti);
}

void PciSdhci::GetBti(GetBtiRequestView request, fdf::Arena& arena,
                      GetBtiCompleter::Sync& completer) {
  zx::bti bti;
  if (zx_status_t status = SdhciGetBti(request->index, &bti); status != ZX_OK) {
    completer.buffer(arena).ReplyError(status);
    return;
  }
  completer.buffer(arena).ReplySuccess(std::move(bti));
}

uint32_t PciSdhci::SdhciGetBaseClock() { return 0; }

void PciSdhci::GetBaseClock(fdf::Arena& arena, GetBaseClockCompleter::Sync& completer) {
  completer.buffer(arena).Reply(SdhciGetBaseClock());
}

uint64_t PciSdhci::SdhciGetQuirks(uint64_t* out_dma_boundary_alignment) {
  *out_dma_boundary_alignment = 0;
  return SDHCI_QUIRK_STRIP_RESPONSE_CRC_PRESERVE_ORDER;
}

void PciSdhci::GetQuirks(fdf::Arena& arena, GetQuirksCompleter::Sync& completer) {
  uint64_t dma_boundary_alignment;
  fuchsia_hardware_sdhci::Quirk quirks{SdhciGetQuirks(&dma_boundary_alignment)};
  completer.buffer(arena).Reply(quirks, dma_boundary_alignment);
}

void PciSdhci::SdhciHwReset() {
  if (!mmio_.has_value()) {
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
}

void PciSdhci::HwReset(fdf::Arena& arena, HwResetCompleter::Sync& completer) {
  SdhciHwReset();
  completer.buffer(arena).Reply();
}

zx_status_t PciSdhci::SdhciVendorSetBusClock(uint32_t frequency_hz) { return ZX_ERR_STOP; }

void PciSdhci::VendorSetBusClock(VendorSetBusClockRequestView request, fdf::Arena& arena,
                                 VendorSetBusClockCompleter::Sync& completer) {
  if (zx_status_t status = SdhciVendorSetBusClock(request->frequency_hz)) {
    completer.buffer(arena).ReplyError(status);
    return;
  }
  completer.buffer(arena).ReplySuccess();
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
                      .set_proto_id(ZX_PROTOCOL_SDHCI)
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
