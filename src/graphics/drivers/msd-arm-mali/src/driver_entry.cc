// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.gpu.magma/cpp/wire.h>
#include <fuchsia/hardware/platform/device/c/banjo.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/ddk/platform-defs.h>
#include <lib/fidl/cpp/wire/arena.h>
#include <lib/fit/thread_safety.h>
#include <zircon/errors.h>
#include <zircon/process.h>
#include <zircon/time.h>
#include <zircon/types.h>

#include <memory>

#include <ddktl/device.h>
#include <ddktl/fidl.h>
#include <ddktl/protocol/empty-protocol.h>

#include "magma_util/platform/zircon/zircon_platform_status.h"
#include "magma_util/short_macros.h"
#include "parent_device_dfv1.h"
#include "platform_logger.h"
#include "src/graphics/drivers/msd-arm-mali/src/parent_device.h"
#include "src/graphics/lib/magma/src/sys_driver_cpp/magma_device_impl.h"
#include "sys_driver_cpp/magma_driver.h"
#include "sys_driver_cpp/magma_system_device.h"

#if MAGMA_TEST_DRIVER
zx_status_t magma_indriver_test(ParentDevice* device);
#endif

class GpuDevice;

using DdkDeviceType =
    ddk::Device<GpuDevice, magma::MagmaDeviceImpl, ddk::Unbindable, ddk::Initializable>;

class GpuDevice : public DdkDeviceType, public ddk::EmptyProtocol<ZX_PROTOCOL_GPU> {
 public:
  explicit GpuDevice(zx_device_t* parent_device) : DdkDeviceType(parent_device) {}

  void DdkInit(ddk::InitTxn txn);
  void DdkUnbind(ddk::UnbindTxn txn);
  void DdkRelease();

  zx_status_t Init();

 private:
  zx_status_t MagmaStart() FIT_REQUIRES(magma_mutex());

  std::unique_ptr<ParentDeviceDFv1> parent_device_;
};

zx_status_t GpuDevice::MagmaStart() {
  set_magma_system_device(magma_driver()->CreateDevice(parent_device_->ToDeviceHandle()));
  if (!magma_system_device())
    return DRET_MSG(ZX_ERR_NO_RESOURCES, "Failed to create device");
  InitSystemDevice();
  return ZX_OK;
}

void GpuDevice::DdkInit(ddk::InitTxn txn) {
  set_zx_device(zxdev());
  txn.Reply(InitChildDevices());
}

void GpuDevice::DdkUnbind(ddk::UnbindTxn txn) {
  // This will tear down client connections and cause them to return errors.
  MagmaStop();
  txn.Reply();
}

void GpuDevice::DdkRelease() {
  MAGMA_LOG(INFO, "Starting device_release");

  delete this;
  MAGMA_LOG(INFO, "Finished device_release");
}

zx_status_t GpuDevice::Init() {
  std::lock_guard<std::mutex> lock(magma_mutex());
  parent_device_ = ParentDeviceDFv1::Create(parent());
  if (!parent_device_) {
    MAGMA_LOG(ERROR, "Failed to create ParentDeviceDFv1");
    return ZX_ERR_INTERNAL;
  }
  set_magma_driver(MagmaDriver::Create());
#if MAGMA_TEST_DRIVER
  DLOG("running magma indriver test");
  set_unit_test_status(magma_indriver_test(parent_device_.get()));
#endif

  zx_status_t status = MagmaStart();
  if (status != ZX_OK)
    return status;

  status = DdkAdd(ddk::DeviceAddArgs("magma_gpu")
                      .set_flags(DEVICE_ADD_NON_BINDABLE)
                      .set_inspect_vmo(magma_driver()->DuplicateInspector()->DuplicateVmo()));
  if (status != ZX_OK)
    return DRET_MSG(status, "device_add failed");
  return ZX_OK;
}

static zx_status_t driver_bind(void* context, zx_device_t* parent) {
  MAGMA_LOG(INFO, "driver_bind: binding\n");
  auto gpu = std::make_unique<GpuDevice>(parent);
  if (!gpu)
    return ZX_ERR_NO_MEMORY;

  zx_status_t status = gpu->Init();
  if (status != ZX_OK) {
    return status;
  }
  // DdkAdd in Init took ownership of device.
  [[maybe_unused]] GpuDevice* ptr = gpu.release();
  return ZX_OK;
}

zx_driver_ops_t msd_driver_ops = []() constexpr {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = driver_bind;
  return ops;
}();

ZIRCON_DRIVER(magma_pdev_gpu, msd_driver_ops, "zircon", "0.1");
