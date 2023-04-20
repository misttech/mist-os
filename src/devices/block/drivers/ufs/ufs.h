// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BLOCK_DRIVERS_UFS_UFS_H_
#define SRC_DEVICES_BLOCK_DRIVERS_UFS_UFS_H_

#include <fuchsia/hardware/block/driver/cpp/banjo.h>
#include <lib/device-protocol/pci.h>
#include <lib/fzl/vmo-mapper.h>
#include <lib/inspect/cpp/inspect.h>

#include <ddktl/device.h>
#include <fbl/condition_variable.h>
#include <fbl/intrusive_double_list.h>
#include <fbl/string_printf.h>

#include "registers.h"
#include "upiu/attributes.h"
#include "upiu/descriptors.h"
#include "upiu/flags.h"

namespace ufs {

constexpr uint32_t kMaxLun = 8;

enum NotifyEvent {
  kInit = 0,
  kReset,
  kPreLinkStartup,
  kPostLinkStartup,
  kSetupTransferUtrl,
  kDeviceInitDone,
  kPrePowerModeChange,
  kPostPowerModeChange,
};

class Ufs;

using HostControllerCallback = fit::function<zx::result<>(NotifyEvent, uint64_t data)>;

using UfsDeviceType = ddk::Device<Ufs, ddk::Initializable>;
class Ufs : public UfsDeviceType {
 public:
  static constexpr char kDriverName[] = "ufs";

  explicit Ufs(zx_device_t *parent, ddk::Pci pci, fdf::MmioBuffer mmio,
               fuchsia_hardware_pci::InterruptMode irq_mode, zx::interrupt irq, zx::bti bti)
      : UfsDeviceType(parent),
        pci_(std::move(pci)),
        mmio_(std::move(mmio)),
        irq_mode_(irq_mode),
        irq_(std::move(irq)),
        bti_(std::move(bti)) {}
  ~Ufs() = default;

  static zx_status_t Bind(void *ctx, zx_device_t *parent);
  zx_status_t AddDevice();

  void DdkInit(ddk::InitTxn txn);
  void DdkRelease();

  inspect::Inspector &inspector() { return inspector_; }
  inspect::Node &inspect_node() { return inspect_node_; }

  void SetHostControllerCallback(HostControllerCallback callback) {
    host_controller_callback_ = std::move(callback);
  }

  bool IsDriverShutdown() const { return driver_shutdown_; }

  // Defines a callback function to perform when a |NotifyEvent| occurs.
  static zx::result<> NotifyEventCallback(NotifyEvent event, uint64_t data);

 private:
  // Initialize the UFS controller and bind the logical units.
  zx_status_t Init();

  ddk::Pci pci_;
  fdf::MmioBuffer mmio_;
  fuchsia_hardware_pci::InterruptMode irq_mode_;
  zx::interrupt irq_;
  zx::bti bti_;
  inspect::Inspector inspector_;
  inspect::Node inspect_node_;

  // Callback function to perform when the host controller is notified.
  HostControllerCallback host_controller_callback_;

  bool driver_shutdown_ = false;
};

}  // namespace ufs

#endif  // SRC_DEVICES_BLOCK_DRIVERS_UFS_UFS_H_
