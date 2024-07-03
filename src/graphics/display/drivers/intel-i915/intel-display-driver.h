// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_INTEL_I915_INTEL_DISPLAY_DRIVER_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_INTEL_I915_INTEL_DISPLAY_DRIVER_H_

#include <lib/ddk/device.h>
#include <lib/zx/result.h>

#include <memory>

#include <ddktl/device.h>

#include "src/graphics/display/drivers/intel-i915/intel-i915.h"

namespace i915 {

class IntelDisplayDriver;
using DeviceType =
    ddk::Device<IntelDisplayDriver, ddk::Initializable, ddk::Unbindable, ddk::Suspendable>;

// Driver instance that binds to the intel-i915 PCI device.
//
// This class is responsible for interfacing with the Fuchsia Driver Framework.
class IntelDisplayDriver : public DeviceType {
 public:
  // Creates an `IntelDisplayDriver` instance, performs short-running
  // initialization of the hardware components and instructs the DDK to publish
  // the device. On success, returns zx::ok() and the ownership of the driver
  // instance is claimed by the driver manager.
  //
  // Long-running initialization is performed in the DdkInit hook.
  static zx::result<> Create(zx_device_t* parent);

  IntelDisplayDriver(zx_device_t* parent, std::unique_ptr<Controller> controller);
  ~IntelDisplayDriver();

  // ddk::Initializable:
  void DdkInit(ddk::InitTxn txn);

  // ddk::Unbindable:
  void DdkUnbind(ddk::UnbindTxn txn);

  // ddk::Device:
  void DdkRelease();

  // ddk::Suspendable:
  void DdkSuspend(ddk::SuspendTxn txn);

  zx::result<ddk::AnyProtocol> GetProtocol(uint32_t proto_id);

  Controller* controller() const { return controller_.get(); }

 private:
  zx::result<> Bind();

  std::unique_ptr<Controller> controller_;

  zx_device_t* gpu_core_device_ = nullptr;
  zx_device_t* display_controller_device_ = nullptr;
};

}  // namespace i915

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_INTEL_I915_INTEL_DISPLAY_DRIVER_H_
