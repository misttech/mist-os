// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "../include/lib/virtio/backends/pci.h"

#include <assert.h>
#include <lib/ddk/debug.h>
#include <lib/device-protocol/pci.h>
#include <lib/zx/handle.h>
#include <lib/zx/interrupt.h>
#include <lib/zx/port.h>
#include <zircon/errors.h>
#include <zircon/status.h>
#include <zircon/syscalls/port.h>

#include <algorithm>

#include <fbl/auto_lock.h>
#include <fbl/mutex.h>
#include <virtio/virtio.h>

namespace virtio {

namespace fpci = fuchsia_hardware_pci;

PciBackend::PciBackend(ddk::Pci pci, fuchsia_hardware_pci::wire::DeviceInfo info)
    : pci_(std::move(pci)), info_(info) {
  snprintf(tag_, sizeof(tag_), "pci[%02x:%02x.%1x]", info_.bus_id, info_.dev_id, info_.func_id);
}

zx_status_t PciBackend::Bind() {
  zx::interrupt interrupt;
  zx_status_t status = zx::port::create(/*options=*/ZX_PORT_BIND_TO_INTERRUPT, &wait_port_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "cannot create wait port: %s", zx_status_get_string(status));
    return status;
  }

  // enable bus mastering
  status = pci().SetBusMastering(true);
  if (status != ZX_OK) {
    zxlogf(ERROR, "cannot enable bus master: %s", zx_status_get_string(status));
    return status;
  }

  status = ConfigureInterruptMode();
  if (status != ZX_OK) {
    zxlogf(ERROR, "cannot configure IRQs: %s", zx_status_get_string(status));
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
  fpci::InterruptMode mode = fpci::InterruptMode::kMsiX;
  uint32_t irq_cnt = 2;
  zx_status_t status = pci().SetInterruptMode(mode, irq_cnt);
  if (status != ZX_OK) {
    mode = fpci::InterruptMode::kLegacy;
    irq_cnt = 1;
    status = pci().SetInterruptMode(mode, irq_cnt);
    if (status != ZX_OK) {
      irq_cnt = 0;
    }
  }

  if (irq_cnt == 0) {
    zxlogf(ERROR, "Failed to configure a virtio IRQ mode: %s", zx_status_get_string(status));
    return status;
  }

  // Legacy only supports 1 IRQ, but for MSI-X we only need 2
  for (uint32_t i = 0; i < irq_cnt; i++) {
    zx::interrupt interrupt = {};
    status = pci().MapInterrupt(i, &interrupt);
    if (status != ZX_OK) {
      zxlogf(ERROR, "Failed to map interrupt %u: %s", i, zx_status_get_string(status));
      return status;
    }

    // Use the interrupt index as the key so we can ack the correct interrupt after
    // a port wait.
    status = interrupt.bind(wait_port_, /*key=*/i, /*options=*/0);
    if (status != ZX_OK) {
      zxlogf(ERROR, "Failed to bind interrupt %u: %s", i, zx_status_get_string(status));
      return status;
    }
    irq_handles().push_back(std::move(interrupt));
  }
  irq_mode() = mode;
  zxlogf(DEBUG, "using %s IRQ mode (irq_cnt = %u)",
         (irq_mode() == fpci::InterruptMode::kMsiX ? "MSI-X" : "legacy"), irq_cnt);
  return ZX_OK;
}

zx::result<uint32_t> PciBackend::WaitForInterrupt() {
  zx_port_packet packet;
  zx_status_t status = wait_port_.wait(zx::deadline_after(zx::msec(100)), &packet);
  if (status != ZX_OK) {
    return zx::error(status);
  }

  return zx::ok(packet.key);
}

void PciBackend::InterruptAck(uint32_t key) {
  ZX_DEBUG_ASSERT(key < irq_handles().size());
  irq_handles()[key].ack();
  if (irq_mode() == fpci::InterruptMode::kLegacy) {
    pci().AckInterrupt();
  }
}

}  // namespace virtio
