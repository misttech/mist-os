// Copyright 2025 Mist Tecnologia Ltda. All rights reserved.
// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <assert.h>
#include <lib/virtio/backends/pci.h>
#include <trace.h>
#include <zircon/errors.h>

#include <algorithm>
#include <utility>

#include <fbl/auto_lock.h>
#include <fbl/mutex.h>
#include <object/pci_interrupt_dispatcher.h>
#include <virtio/virtio.h>

#include <ktl/enforce.h>

#define LOCAL_TRACE 0

namespace virtio {

PciBackend::PciBackend(KernelHandle<PciDeviceDispatcher> pci, zx_pcie_device_info_t info)
    : pci_(ktl::move(pci)), info_(info) {
  snprintf(tag_, sizeof(tag_), "pci[%02x:%02x.%1x]", info_.bus_id, info_.dev_id, info_.func_id);
}

zx_status_t PciBackend::Bind() {
  zx_rights_t rights;
  zx_status_t status = PortDispatcher::Create(ZX_PORT_BIND_TO_INTERRUPT, &wait_port_, &rights);
  if (status != ZX_OK) {
    LTRACEF("cannot create wait port: %d\n", status);
    return status;
  }

  // enable bus mastering
  status = pci().EnableBusMaster(true);
  if (status != ZX_OK) {
    LTRACEF("cannot enable bus master: %d\n", status);
    return status;
  }

  status = ConfigureInterruptMode();
  if (status != ZX_OK) {
    LTRACEF("cannot configure IRQs: %d\n", status);
    return status;
  }

  return Init();
}

// Virtio supports both a legacy INTx IRQ as well as MSI-X. In the former case,
// a driver is required to read the ISR_STATUS register to determine what sort
// of event has happened. This can be an expensive operation depending on the
// hypervisor / emulation environment. For MSI-X a device 'should' support 2 or
// more vector table entries, but is not required to. Since we only have one IRQ
// worker in the backends at this time it's not that important that we allocate
// a vector per ring, so for now the ideal is roughly two vectors, one being for
// config changes and the other for rings.
zx_status_t PciBackend::ConfigureInterruptMode() {
  // This looks a lot like something ConfigureInterruptMode was designed for, but
  // since we have a specific requirement to use MSI-X if and only if we have 2
  // vectors it means rolling it by hand.
  pcie_irq_mode_t mode = PCIE_IRQ_MODE_MSI_X;
  uint32_t irq_cnt = 2;
  zx_status_t status = pci().SetIrqMode(mode, irq_cnt);
  if (status != ZX_OK) {
    mode = PCIE_IRQ_MODE_LEGACY;
    irq_cnt = 1;
    status = pci().SetIrqMode(mode, irq_cnt);
    if (status != ZX_OK) {
      irq_cnt = 0;
    }
  }

  if (irq_cnt == 0) {
    LTRACEF("Failed to configure a virtio IRQ mode: %d\n", status);
    return status;
  }

  // Legacy only supports 1 IRQ, but for MSI-X we only need 2
  for (uint32_t i = 0; i < irq_cnt; i++) {
    KernelHandle<InterruptDispatcher> interrupt;
    zx_rights_t rights;
    status = pci().MapInterrupt(i, &interrupt, &rights);
    if (status != ZX_OK) {
      LTRACEF("Failed to map interrupt %u: %d\n", i, status);
      return status;
    }

    // Use the interrupt index as the key so we can ack the correct interrupt after
    // a port wait.
    status = interrupt.dispatcher()->Bind(wait_port_.dispatcher(), i);
    if (status != ZX_OK) {
      LTRACEF("Failed to bind interrupt %u: %d\n", i, status);
      return status;
    }
    fbl::AllocChecker ac;
    irq_handles().push_back(ktl::move(interrupt), &ac);
    if (!ac.check()) {
      LTRACEF("Failed to allocate interrupt %u\n", i);
      return ZX_ERR_NO_MEMORY;
    }
  }
  irq_mode() = mode;
  LTRACEF("using %s IRQ mode (irq_cnt = %u)\n",
          (irq_mode() == PCIE_IRQ_MODE_MSI_X ? "MSI-X" : "legacy"), irq_cnt);
  return ZX_OK;
}

zx::result<uint32_t> PciBackend::WaitForInterrupt() {
  zx_port_packet_t pp;
  zx_status_t st = wait_port_.dispatcher()->Dequeue(Deadline::after_mono(ZX_MSEC(100)), &pp);
  if (st != ZX_OK) {
    return zx::error(st);
  }
  return zx::ok(pp.key);
}

void PciBackend::InterruptAck(uint32_t key) {
  ZX_DEBUG_ASSERT(key < irq_handles().size());
  ktl::ignore = irq_handles()[key].dispatcher()->Ack();
}

}  // namespace virtio
