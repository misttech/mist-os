// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "controller.h"

#include <inttypes.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/ddk/io-buffer.h>
#include <lib/ddk/phys-iter.h>
#include <lib/device-protocol/pci.h>
#include <lib/zx/clock.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/param.h>
#include <threads.h>
#include <unistd.h>
#include <zircon/assert.h>
#include <zircon/listnode.h>
#include <zircon/syscalls.h>
#include <zircon/types.h>

#include <fbl/alloc_checker.h>
#include <fbl/auto_lock.h>

#include "pci-bus.h"
#include "sata.h"

namespace ahci {

//clang-format on

// TODO(sron): Check return values from bus_->RegRead() and RegWrite().
// Handle properly for buses that may by unplugged at runtime.
uint32_t Controller::RegRead(size_t offset) {
  uint32_t val = 0;
  bus_->RegRead(offset, &val);
  return val;
}

zx_status_t Controller::RegWrite(size_t offset, uint32_t val) {
  return bus_->RegWrite(offset, val);
}

void Controller::AhciEnable() {
  uint32_t ghc = RegRead(kHbaGlobalHostControl);
  if (ghc & AHCI_GHC_AE)
    return;
  for (int i = 0; i < 5; i++) {
    ghc |= AHCI_GHC_AE;
    RegWrite(kHbaGlobalHostControl, ghc);
    ghc = RegRead(kHbaGlobalHostControl);
    if (ghc & AHCI_GHC_AE)
      return;
    usleep(10 * 1000);
  }
}

zx_status_t Controller::HbaReset() {
  // AHCI 1.3: Software may perform an HBA reset prior to initializing the controller
  uint32_t ghc = RegRead(kHbaGlobalHostControl);
  ghc |= AHCI_GHC_AE;
  RegWrite(kHbaGlobalHostControl, ghc);
  ghc |= AHCI_GHC_HR;
  RegWrite(kHbaGlobalHostControl, ghc);
  // reset should complete within 1 second
  zx_status_t status = bus_->WaitForClear(kHbaGlobalHostControl, AHCI_GHC_HR, zx::sec(1));
  if (status) {
    zxlogf(ERROR, "ahci: hba reset timed out");
  }
  return status;
}

zx_status_t Controller::SetDevInfo(uint32_t portnr, SataDeviceInfo* devinfo) {
  if (portnr >= AHCI_MAX_PORTS) {
    return ZX_ERR_OUT_OF_RANGE;
  }
  ports_[portnr].SetDevInfo(devinfo);
  return ZX_OK;
}

void Controller::Queue(uint32_t portnr, SataTransaction* txn) {
  ZX_DEBUG_ASSERT(portnr < AHCI_MAX_PORTS);
  Port* port = &ports_[portnr];
  zx_status_t status = port->Queue(txn);
  if (status == ZX_OK) {
    zxlogf(TRACE, "ahci.%u: Queue txn %p offset_dev 0x%" PRIx64 " length 0x%x", port->num(), txn,
           txn->bop.rw.offset_dev, txn->bop.rw.length);
    // hit the worker thread
    sync_completion_signal(&worker_completion_);
  } else {
    zxlogf(INFO, "ahci.%u: Failed to queue txn %p: %s", port->num(), txn,
           zx_status_get_string(status));
    // TODO: close transaction.
  }
}

void Controller::DdkRelease() {
  Shutdown();
  delete this;
}

bool Controller::ShouldExit() {
  fbl::AutoLock lock(&lock_);
  return threads_should_exit_;
}

// worker thread

int Controller::WorkerLoop() {
  Port* port;
  for (;;) {
    // iterate all the ports and run or complete commands
    bool port_active = false;
    for (uint32_t i = 0; i < AHCI_MAX_PORTS; i++) {
      port = &ports_[i];

      // Complete commands first.
      bool txns_in_progress = port->Complete();
      // Process queued txns.
      bool txns_added = port->ProcessQueued();
      port_active |= txns_in_progress || txns_added;
    }

    // Exit only when there are no more transactions in flight.
    if ((!port_active) && ShouldExit()) {
      return 0;
    }

    // Wait here until more commands are queued, or a port becomes idle.
    sync_completion_wait(&worker_completion_, ZX_TIME_INFINITE);
    sync_completion_reset(&worker_completion_);
  }
}

// irq handler:

int Controller::IrqLoop() {
  for (;;) {
    zx_status_t status = bus_->InterruptWait();
    if (status != ZX_OK) {
      if (!ShouldExit()) {
        zxlogf(ERROR, "ahci: Error waiting for interrupt: %s", zx_status_get_string(status));
      }
      return 0;
    }
    // mask hba interrupts while interrupts are being handled
    uint32_t ghc = RegRead(kHbaGlobalHostControl);
    RegWrite(kHbaGlobalHostControl, ghc & ~AHCI_GHC_IE);

    // handle interrupt for each port
    uint32_t is = RegRead(kHbaInterruptStatus);
    RegWrite(kHbaInterruptStatus, is);
    for (uint32_t i = 0; is && i < AHCI_MAX_PORTS; i++) {
      if (is & 0x1) {
        bool txn_handled = ports_[i].HandleIrq();
        if (txn_handled) {
          // hit the worker thread to complete commands
          sync_completion_signal(&worker_completion_);
        }
      }
      is >>= 1;
    }

    // unmask hba interrupts
    ghc = RegRead(kHbaGlobalHostControl);
    RegWrite(kHbaGlobalHostControl, ghc | AHCI_GHC_IE);
  }
}

// implement device protocol:

void Controller::DdkInit(ddk::InitTxn txn) {
  // The drive initialization has numerous error conditions. Wrap the initialization here to ensure
  // we always call txn.Reply() in any outcome.
  zx_status_t status = Init();
  if (status != ZX_OK) {
    zxlogf(ERROR, "ahci: Driver initialization failed: %s", zx_status_get_string(status));
  }
  txn.Reply(status);
}

zx_status_t Controller::Init() {
  zx_status_t status;
  if ((status = LaunchIrqAndWorkerThreads()) != ZX_OK) {
    zxlogf(ERROR, "ahci: Failed to start controller irq and worker threads: %s",
           zx_status_get_string(status));
    return status;
  }

  // reset
  HbaReset();

  // enable ahci mode
  AhciEnable();

  const uint32_t capabilities = RegRead(kHbaCapabilities);
  const bool use_command_queue = capabilities & AHCI_CAP_NCQ;
  const uint32_t max_command_tag = (capabilities >> 8) & 0x1f;
  inspect_node_ = inspector_.GetRoot().CreateChild(kDriverName);
  inspect_node_.RecordBool("native_command_queuing", use_command_queue);
  inspect_node_.RecordUint("max_command_tag", max_command_tag);

  // count number of ports
  uint32_t port_map = RegRead(kHbaPortsImplemented);

  // initialize ports
  for (uint32_t i = 0; i < AHCI_MAX_PORTS; i++) {
    if (!(port_map & (1u << i)))
      continue;  // port not implemented
    status = ports_[i].Configure(i, bus_.get(), kHbaPorts, max_command_tag);
    if (status != ZX_OK) {
      zxlogf(ERROR, "ahci: Failed to configure port %u: %s", i, zx_status_get_string(status));
      return status;
    }
  }

  // clear hba interrupts
  RegWrite(kHbaInterruptStatus, RegRead(kHbaInterruptStatus));

  // enable hba interrupts
  uint32_t ghc = RegRead(kHbaGlobalHostControl);
  ghc |= AHCI_GHC_IE;
  RegWrite(kHbaGlobalHostControl, ghc);

  // this part of port init happens after enabling interrupts in ghc
  for (uint32_t i = 0; i < AHCI_MAX_PORTS; i++) {
    Port* port = &ports_[i];
    if (!(port->port_implemented()))
      continue;

    // enable port
    port->Enable();

    // enable interrupts
    port->RegWrite(kPortInterruptEnable, AHCI_PORT_INT_MASK);

    // reset port
    port->Reset();

    // FIXME proper layering?
    if (port->RegRead(kPortSataStatus) & AHCI_PORT_SSTS_DET_PRESENT) {
      port->set_device_present(true);
      if (port->RegRead(kPortSignature) == AHCI_PORT_SIG_SATA) {
        zx_status_t status = SataDevice::Bind(this, port->num(), use_command_queue);
        if (status != ZX_OK) {
          zxlogf(ERROR, "ahci: Failed to add SATA device at port %u: %s", port->num(),
                 zx_status_get_string(status));
          return status;
        }
      }
    }
  }

  return ZX_OK;
}

zx::result<std::unique_ptr<Controller>> Controller::CreateWithBus(zx_device_t* parent,
                                                                  std::unique_ptr<Bus> bus) {
  fbl::AllocChecker ac;
  std::unique_ptr<Controller> controller(new (&ac) Controller(parent));
  if (!ac.check()) {
    zxlogf(ERROR, "ahci: out of memory");
    return zx::error(ZX_ERR_NO_MEMORY);
  }
  zx_status_t status = bus->Configure(parent);
  if (status != ZX_OK) {
    zxlogf(ERROR, "ahci: failed to configure host bus");
    return zx::error(status);
  }
  controller->bus_ = std::move(bus);
  return zx::ok(std::move(controller));
}

zx_status_t Controller::LaunchIrqAndWorkerThreads() {
  zx_status_t status = irq_thread_.CreateWithName(IrqThread, this, "ahci-irq");
  if (status != ZX_OK) {
    zxlogf(ERROR, "ahci: Error creating irq thread: %s", zx_status_get_string(status));
    return status;
  }
  status = worker_thread_.CreateWithName(WorkerThread, this, "ahci-worker");
  if (status != ZX_OK) {
    zxlogf(ERROR, "ahci: Error creating worker thread: %s", zx_status_get_string(status));
    return status;
  }
  return ZX_OK;
}

void Controller::Shutdown() {
  {
    fbl::AutoLock lock(&lock_);
    threads_should_exit_ = true;
  }

  // Signal the worker thread.
  sync_completion_signal(&worker_completion_);
  worker_thread_.Join();

  // Signal the interrupt thread to exit.
  bus_->InterruptCancel();
  irq_thread_.Join();
}

// implement driver object:

zx_status_t Controller::Bind(void* ctx, zx_device_t* parent) {
  if (AHCI_PAGE_SIZE != zx_system_get_page_size()) {
    zxlogf(ERROR, "ahci: System page size of %u does not match expected page size of %u\n",
           zx_system_get_page_size(), AHCI_PAGE_SIZE);
    return ZX_ERR_INTERNAL;
  }

  std::unique_ptr<Controller> controller;
  {
    fbl::AllocChecker ac;
    std::unique_ptr<Bus> bus(new (&ac) PciBus(parent));
    if (!ac.check()) {
      zxlogf(ERROR, "ahci: Failed to allocate memory for PciBus.");
      return ZX_ERR_NO_MEMORY;
    }

    zx::result result = Controller::CreateWithBus(parent, std::move(bus));
    if (result.is_error()) {
      zxlogf(ERROR, "ahci: Failed to create AHCI controller: %s",
             zx_status_get_string(result.status_value()));
      return result.status_value();
    }
    controller = std::move(result.value());
  }

  zx_status_t status = controller->AddDevice();
  if (status != ZX_OK) {
    return status;
  }

  // The DriverFramework now owns driver.
  controller.release();
  return ZX_OK;
}

zx_status_t Controller::AddDevice() {
  zx_status_t status = DdkAdd(ddk::DeviceAddArgs(kDriverName)
                                  .set_flags(DEVICE_ADD_NON_BINDABLE)
                                  .set_inspect_vmo(inspector_.DuplicateVmo()));
  if (status != ZX_OK) {
    zxlogf(ERROR, "ahci: Error in DdkAdd: %s", zx_status_get_string(status));
  }
  return status;
}

constexpr zx_driver_ops_t ahci_driver_ops = []() {
  zx_driver_ops_t driver = {};
  driver.version = DRIVER_OPS_VERSION;
  driver.bind = Controller::Bind;
  return driver;
}();

}  // namespace ahci

ZIRCON_DRIVER(ahci, ahci::ahci_driver_ops, "zircon", "0.1");
