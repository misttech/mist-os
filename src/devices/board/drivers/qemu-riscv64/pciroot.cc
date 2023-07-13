// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "pciroot.h"

#include <fidl/fuchsia.hardware.pci/cpp/wire.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/fidl.h>
#include <fuchsia/hardware/pciroot/c/banjo.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/driver.h>
#include <lib/ddk/platform-defs.h>
#include <lib/pci/root_host.h>
#include <lib/zx/result.h>
#include <lib/zx/vmo.h>
#include <stdint.h>
#include <zircon/errors.h>
#include <zircon/rights.h>
#include <zircon/status.h>
#include <zircon/syscalls/types.h>

#include <array>
#include <limits>

#include <fbl/alloc_checker.h>
#include <region-alloc/region-alloc.h>

#include "qemu-riscv64.h"

namespace fpci = fuchsia_hardware_pci::wire;
namespace board_qemu_riscv64 {

namespace {

// PCIE IRQs are [32, 35] from qemu/hw/riscv/virt.h.
constexpr uint8_t kVirtPcieIrqBase = 0x20;
constexpr uint8_t kVirtPcieIrqCount = 0x4;

// Values per the `virt_memmap` MemMapEntry in qemu/hw/riscv/virt.c.
constexpr ralloc_region_t kVirtPcieEcam = {.base = 0x3000'0000, .size = 0x1000'0000};
constexpr ralloc_region_t kVirtPcieMmio = {.base = 0x4000'0000, .size = 0x4000'0000};
constexpr ralloc_region_t kVirtPciePio = {.base = 0x300'0000, .size = 0x10000};
constexpr uint32_t kBytesPerPciBus =
    fpci::kExtendedConfigSize * FUNCTIONS_PER_DEVICE * DEVICES_PER_BUS;
constexpr uint16_t kEndBusNumber = (kVirtPcieEcam.size / kBytesPerPciBus) - 1;
constexpr char kPcirootName[] = "PCI0";

constexpr McfgAllocation kVirtPcieMcfg = {
    .address = kVirtPcieEcam.base,
    .pci_segment = 0,
    .start_bus_number = 0,
    .end_bus_number = kEndBusNumber,
};
}  // namespace

zx_status_t QemuRiscv64Pciroot::PcirootGetBti(uint32_t bdf, uint32_t index, zx::bti* bti) {
  return iommu_.GetBti(/*iommu_index=*/index, /*bti_id=*/bdf, /*out_handle=*/bti);
}

zx_status_t QemuRiscv64Pciroot::PcirootGetPciPlatformInfo(pci_platform_info_t* info) {
  info->start_bus_num = kVirtPcieMcfg.start_bus_number;
  info->end_bus_num = kVirtPcieMcfg.end_bus_number;
  info->segment_group = kVirtPcieMcfg.pci_segment;
  info->legacy_irqs_list = interrupts_.data();
  info->legacy_irqs_count = interrupts_.size();
  info->irq_routing_list = irq_routing_entries_.data();
  info->irq_routing_count = irq_routing_entries_.size();
  info->acpi_bdfs_count = 0;
  strncpy(info->name, kPcirootName, sizeof(kPcirootName));

  zx::vmo ecam;
  if (zx_status_t status = context_.ecam.duplicate(ZX_RIGHT_SAME_RIGHTS, &ecam); status != ZX_OK) {
    zxlogf(WARNING, "couldn't duplicate ecam handle: %s", zx_status_get_string(status));
  }

  info->ecam_vmo = ecam.release();
  return ZX_OK;
}

zx::result<> QemuRiscv64Pciroot::Create(PciRootHost* root_host, QemuRiscv64Pciroot::Context ctx,
                                        zx_device_t* parent, const char* name) {
  auto pciroot = std::unique_ptr<QemuRiscv64Pciroot>(
      new QemuRiscv64Pciroot(root_host, std::move(ctx), parent, name));
  if (auto result = pciroot->CreateInterrupts(); result.is_error()) {
    return result.take_error();
  }

  zx_status_t status =
      pciroot->DdkAdd(ddk::DeviceAddArgs(name).set_inspect_vmo(pciroot->inspect().DuplicateVmo()));
  if (status == ZX_OK) {
    [[maybe_unused]] auto ptr = pciroot.release();
  }

  return zx::make_result(status);
}

zx::result<> QemuRiscv64::PcirootInit() {
  zx::unowned_resource root_resource(get_root_resource(parent()));

  pci_root_host_.mcfgs().push_back(kVirtPcieMcfg);
  pci_root_host_.Io().AddRegion(kVirtPciePio);
  pci_root_host_.Mmio64().AddRegion(kVirtPcieMmio);

  QemuRiscv64Pciroot::Context context{};
  zx_status_t status =
      zx::vmo::create_physical(/*resource=*/*root_resource, /*paddr=*/kVirtPcieEcam.base,
                               /*size=*/kVirtPcieEcam.size, /*result=*/&context.ecam);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to allocate ecam vmo for [%#lx, %#lx): %s", kVirtPcieEcam.base,
           kVirtPcieEcam.base + kVirtPcieEcam.size, zx_status_get_string(status));
    return zx::error(status);
  }

  return QemuRiscv64Pciroot::Create(&pci_root_host_, std::move(context), parent(), kPcirootName);
}

zx::result<> QemuRiscv64Pciroot::CreateInterrupts() {
  zx::unowned_resource root_resource(get_root_resource(parent()));

  for (uint32_t vector_offset = 0; vector_offset < kVirtPcieIrqCount; vector_offset++) {
    zx::interrupt interrupt;
    zx_status_t status = zx::interrupt::create(/*resource=*/*root_resource,
                                               /*vector=*/kVirtPcieIrqBase + vector_offset,
                                               /*options=*/0, /*result=*/&interrupt);
    if (status != ZX_OK) {
      return zx::error(status);
    }

    interrupts_.push_back(pci_legacy_irq_t{
        .interrupt = interrupt.release(),
        .vector = kVirtPcieIrqBase + vector_offset,
    });

    // All QEMU devices are plugged into the root port and use a pre-defined pin swizzle.
    for (uint8_t device_id = 0; device_id < DEVICES_PER_BUS; device_id++) {
      pci_irq_routing_entry_t entry = {.port_device_id = PCI_IRQ_ROUTING_NO_PARENT,
                                       .port_function_id = PCI_IRQ_ROUTING_NO_PARENT,
                                       .device_id = device_id};
      for (uint32_t pin = 0; pin < PINS_PER_FUNCTION; pin++) {
        entry.pins[pin] = kVirtPcieIrqBase + ((pin + device_id) % PINS_PER_FUNCTION);
      }
      irq_routing_entries_.push_back(entry);
    }
  }

  return zx::ok();
}

}  // namespace board_qemu_riscv64
