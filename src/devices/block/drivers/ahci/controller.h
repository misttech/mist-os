// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BLOCK_DRIVERS_AHCI_CONTROLLER_H_
#define SRC_DEVICES_BLOCK_DRIVERS_AHCI_CONTROLLER_H_

#include <lib/inspect/cpp/inspect.h>
#include <lib/sync/completion.h>
#include <lib/zx/time.h>
#include <zircon/types.h>

#include <memory>

#include <fbl/condition_variable.h>
#include <fbl/mutex.h>

#include "ahci.h"
#include "bus.h"
#include "port.h"

namespace ahci {

struct ThreadWrapper {
  thrd_t thread;
  bool created = false;

  ~ThreadWrapper() { ZX_DEBUG_ASSERT(created == false); }

  zx_status_t CreateWithName(thrd_start_t entry, void* arg, const char* name) {
    ZX_DEBUG_ASSERT(created == false);
    if (thrd_create_with_name(&thread, entry, arg, name) == thrd_success) {
      created = true;
      return ZX_OK;
    }
    return ZX_ERR_NO_MEMORY;
  }

  void Join() {
    if (!created)
      return;
    thrd_join(thread, nullptr);
    created = false;
  }
};

class Controller;
using ControllerDeviceType = ddk::Device<Controller, ddk::Initializable>;
class Controller : public ControllerDeviceType {
 public:
  static constexpr char kDriverName[] = "ahci";

  // Test function: Create a new AHCI Controller with a caller-provided host bus interface.
  static zx::result<std::unique_ptr<Controller>> CreateWithBus(zx_device_t* parent,
                                                               std::unique_ptr<Bus> bus);

  explicit Controller(zx_device_t* parent) : ControllerDeviceType(parent) {}
  ~Controller() = default;

  DISALLOW_COPY_ASSIGN_AND_MOVE(Controller);

  static zx_status_t Bind(void* ctx, zx_device_t* parent);
  // Invokes DdkAdd().
  zx_status_t AddDevice();

  void DdkInit(ddk::InitTxn txn);
  void DdkRelease();

  // Read or write a 32-bit AHCI controller reg. Endinaness is corrected.
  uint32_t RegRead(size_t offset);
  zx_status_t RegWrite(size_t offset, uint32_t val);

  // Create irq and worker threads.
  zx_status_t LaunchIrqAndWorkerThreads();

  // Release all resources.
  // Not used in DDK lifecycle where Release() is called.
  void Shutdown() __TA_EXCLUDES(lock_);

  zx_status_t HbaReset();
  void AhciEnable();

  zx_status_t SetDevInfo(uint32_t portnr, SataDeviceInfo* devinfo);
  void Queue(uint32_t portnr, SataTransaction* txn);

  void SignalWorker() { sync_completion_signal(&worker_completion_); }

  inspect::Inspector& inspector() { return inspector_; }
  inspect::Node& inspect_node() { return inspect_node_; }

  Bus* bus() { return bus_.get(); }
  Port* port(uint32_t portnr) { return &ports_[portnr]; }

 private:
  static int WorkerThread(void* arg) { return static_cast<Controller*>(arg)->WorkerLoop(); }
  static int IrqThread(void* arg) { return static_cast<Controller*>(arg)->IrqLoop(); }
  int WorkerLoop();
  int IrqLoop();

  // Initialize controller and detect devices.
  zx_status_t Init();

  bool ShouldExit() __TA_EXCLUDES(lock_);

  inspect::Inspector inspector_;
  inspect::Node inspect_node_;

  fbl::Mutex lock_;
  bool threads_should_exit_ __TA_GUARDED(lock_) = false;

  ThreadWrapper irq_thread_;
  ThreadWrapper worker_thread_;

  sync_completion_t worker_completion_;

  std::unique_ptr<Bus> bus_;
  Port ports_[AHCI_MAX_PORTS];
};

}  // namespace ahci

#endif  // SRC_DEVICES_BLOCK_DRIVERS_AHCI_CONTROLLER_H_
