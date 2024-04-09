// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "controller.h"

#include <inttypes.h>
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
    FDF_LOG(ERROR, "HBA reset timed out");
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
    FDF_LOG(TRACE, "ahci.%u: Queue txn %p offset_dev 0x%" PRIx64 " length 0x%x", port->num(), txn,
            txn->bop.rw.offset_dev, txn->bop.rw.length);
    // hit the worker loop
    worker_event_completion_.Signal();
  } else {
    FDF_LOG(INFO, "ahci.%u: Failed to queue txn %p: %s", port->num(), txn,
            zx_status_get_string(status));
    // TODO: close transaction.
  }
}

void Controller::PrepareStop(fdf::PrepareStopCompleter completer) {
  Shutdown();
  completer(zx::ok());
}

bool Controller::ShouldExit() {
  fbl::AutoLock lock(&lock_);
  return threads_should_exit_;
}

void Controller::WorkerLoop() {
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
      return;
    }

    // Wait here until more commands are queued, or a port becomes idle.
    worker_event_completion_.Wait();
    worker_event_completion_.Reset();
  }
}

// irq handler:

int Controller::IrqLoop() {
  for (;;) {
    zx_status_t status = bus_->InterruptWait();
    if (status != ZX_OK) {
      if (!ShouldExit()) {
        FDF_LOG(ERROR, "Error waiting for interrupt: %s", zx_status_get_string(status));
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
          // hit the worker loop to complete commands
          worker_event_completion_.Signal();
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

zx_status_t Controller::Init() {
  zx_status_t status;
  if ((status = LaunchIrqAndWorkerThreads()) != ZX_OK) {
    FDF_LOG(ERROR, "Failed to start controller irq and worker threads: %s",
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
      FDF_LOG(ERROR, "Failed to configure port %u: %s", i, zx_status_get_string(status));
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
        zx::result<std::unique_ptr<SataDevice>> device =
            SataDevice::Bind(this, port->num(), use_command_queue);
        if (device.is_error()) {
          FDF_LOG(ERROR, "Failed to add SATA device at port %u: %s", port->num(),
                  device.status_string());
          return device.status_value();
        }
        sata_devices_.push_back(*std::move(device));
      }
    }
  }

  return ZX_OK;
}

zx_status_t Controller::LaunchIrqAndWorkerThreads() {
  // TODO(b/324291694): Switch to using DF's dispatcher thread.
  zx_status_t status = irq_thread_.CreateWithName(IrqThread, this, "ahci-irq");
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Error creating irq thread: %s", zx_status_get_string(status));
    return status;
  }

  auto dispatcher = fdf::SynchronizedDispatcher::Create(
      fdf::SynchronizedDispatcher::Options::kAllowSyncCalls, "ahci-worker",
      [&](fdf_dispatcher_t*) { worker_shutdown_completion_.Signal(); });
  if (dispatcher.is_error()) {
    FDF_LOG(ERROR, "Failed to create dispatcher: %s",
            zx_status_get_string(dispatcher.status_value()));
    return dispatcher.status_value();
  }
  worker_dispatcher_ = *std::move(dispatcher);

  status = async::PostTask(worker_dispatcher_.async_dispatcher(), [this] { WorkerLoop(); });
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Error creating worker loop: %s", zx_status_get_string(status));
    return status;
  }
  return ZX_OK;
}

void Controller::Shutdown() {
  if (worker_dispatcher_.get()) {
    {
      fbl::AutoLock lock(&lock_);
      threads_should_exit_ = true;
    }

    // Signal the worker loop.
    worker_event_completion_.Signal();
    worker_dispatcher_.ShutdownAsync();
    worker_shutdown_completion_.Wait();
  }

  // Signal the interrupt thread to exit.
  bus_->InterruptCancel();
  irq_thread_.Join();
}

zx::result<std::unique_ptr<Bus>> Controller::CreateBus() {
  auto pci_client_end = incoming()->Connect<fuchsia_hardware_pci::Service::Device>("pci");
  if (!pci_client_end.is_ok()) {
    FDF_LOG(ERROR, "Failed to connect to PCI device service: %s", pci_client_end.status_string());
    return pci_client_end.take_error();
  }
  auto pci = fidl::WireSyncClient(*std::move(pci_client_end));

  fbl::AllocChecker ac;
  auto bus = fbl::make_unique_checked<PciBus>(&ac, std::move(pci));
  if (!ac.check()) {
    FDF_LOG(ERROR, "Failed to allocate memory for bus.");
    return zx::error(ZX_ERR_NO_MEMORY);
  }
  return zx::ok(std::move(bus));
}

zx::result<> Controller::Start() {
  parent_node_.Bind(std::move(node()));

  if (AHCI_PAGE_SIZE != zx_system_get_page_size()) {
    FDF_LOG(ERROR, "System page size of %u does not match expected page size of %u\n",
            zx_system_get_page_size(), AHCI_PAGE_SIZE);
    return zx::error(ZX_ERR_INTERNAL);
  }

  zx::result<std::unique_ptr<Bus>> bus = CreateBus();
  if (bus.is_error()) {
    return bus.take_error();
  }
  bus_ = *std::move(bus);

  zx_status_t status = bus_->Configure();
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Failed to configure host bus");
    return zx::error(status);
  }

  auto inspect_sink = incoming()->Connect<fuchsia_inspect::InspectSink>();
  if (inspect_sink.is_error() || !inspect_sink->is_valid()) {
    FDF_LOG(ERROR, "Failed to connect to inspect sink: %s", inspect_sink.status_string());
    return inspect_sink.take_error();
  }
  exposed_inspector_.emplace(inspect::ComponentInspector(
      dispatcher(), {.inspector = inspector(), .client_end = std::move(inspect_sink.value())}));

  auto [controller_client_end, controller_server_end] =
      fidl::Endpoints<fuchsia_driver_framework::NodeController>::Create();
  auto [node_client_end, node_server_end] =
      fidl::Endpoints<fuchsia_driver_framework::Node>::Create();

  node_controller_.Bind(std::move(controller_client_end));
  root_node_.Bind(std::move(node_client_end));

  fidl::Arena arena;

  const auto args =
      fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena).name(arena, name()).Build();

  auto result =
      parent_node_->AddChild(args, std::move(controller_server_end), std::move(node_server_end));
  if (!result.ok()) {
    FDF_LOG(ERROR, "Failed to add child: %s", result.status_string());
    return zx::error(result.status());
  }

  status = Init();
  if (status != ZX_OK) {
    FDF_LOG(ERROR, "Driver initialization failed: %s", zx_status_get_string(status));
    return zx::error(status);
  }
  return zx::ok();
}

}  // namespace ahci
