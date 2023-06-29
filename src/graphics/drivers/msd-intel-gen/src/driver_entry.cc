// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.gpu.magma/cpp/wire.h>
#include <fuchsia/hardware/intelgpucore/cpp/banjo.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/zx/channel.h>
#include <lib/zx/resource.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <zircon/process.h>
#include <zircon/types.h>

#include <atomic>
#include <set>
#include <thread>

#include <ddktl/device.h>
#include <ddktl/fidl.h>
#include <ddktl/protocol/empty-protocol.h>

#include "magma_util/dlog.h"
#include "msd_defs.h"
#include "msd_intel_pci_device.h"
#include "src/graphics/lib/magma/src/magma_util/platform/zircon/zircon_platform_status.h"
#include "src/graphics/lib/magma/src/sys_driver/dfv1/magma_device_impl.h"

#if MAGMA_TEST_DRIVER
zx_status_t magma_indriver_test(magma::PlatformPciDevice* platform_device);

#endif

class IntelDevice;

using DdkDeviceType =
    ddk::Device<IntelDevice, msd::MagmaDeviceImpl, ddk::Unbindable, ddk::Initializable>;
class IntelDevice : public DdkDeviceType, public ddk::EmptyProtocol<ZX_PROTOCOL_GPU> {
 public:
  explicit IntelDevice(zx_device_t* parent_device) : DdkDeviceType(parent_device) {}

  int MagmaStart() FIT_REQUIRES(magma_mutex()) {
    DLOG("magma_start");

    set_magma_system_device(msd::MagmaSystemDevice::Create(
        magma_driver(),
        magma_driver()->CreateDevice(reinterpret_cast<msd::DeviceHandle*>(&gpu_core_protocol_))));
    if (!magma_system_device())
      return DRET_MSG(ZX_ERR_NO_RESOURCES, "Failed to create device");

    DLOG("Created device %p", magma_system_device());
    InitSystemDevice();

    return ZX_OK;
  }

  void DdkInit(ddk::InitTxn txn);
  void DdkUnbind(ddk::UnbindTxn txn);
  void DdkRelease();

  zx_status_t Init();

 private:
  intel_gpu_core_protocol_t gpu_core_protocol_;
};

void IntelDevice::DdkInit(ddk::InitTxn txn) {
  set_zx_device(zxdev());
  txn.Reply(InitChildDevices());
}

void IntelDevice::DdkUnbind(ddk::UnbindTxn txn) {
  // This will tear down client connections and cause them to return errors.
  MagmaStop();
  txn.Reply();
}

void IntelDevice::DdkRelease() {
  MAGMA_LOG(INFO, "Starting device_release");

  delete this;
  MAGMA_LOG(INFO, "Finished device_release");
}

zx_status_t IntelDevice::Init() {
  ddk::IntelGpuCoreProtocolClient gpu_core_client;
  zx_status_t status =
      ddk::IntelGpuCoreProtocolClient::CreateFromDevice(parent(), &gpu_core_client);
  if (status != ZX_OK)
    return DRET_MSG(status, "device_get_protocol failed: %d", status);

  gpu_core_client.GetProto(&gpu_core_protocol_);

  std::lock_guard<std::mutex> lock(magma_mutex());
  set_magma_driver(msd::Driver::Create());
#if MAGMA_TEST_DRIVER
  DLOG("running magma indriver test");
  {
    auto platform_device = MsdIntelPciDevice::CreateShim(&gpu_core_protocol_);
    set_unit_test_status(magma_indriver_test(platform_device.get()));
  }
#endif

  status = MagmaStart();
  if (status != ZX_OK)
    return status;

  status = DdkAdd(ddk::DeviceAddArgs("magma_gpu").set_flags(DEVICE_ADD_NON_BINDABLE));
  if (status != ZX_OK)
    return DRET_MSG(status, "device_add failed");
  return ZX_OK;
}

static zx_status_t sysdrv_bind(void* ctx, zx_device_t* parent) {
  DLOG("sysdrv_bind start zx_device %p", parent);
  magma::PlatformBusMapper::SetInfoResource(zx::unowned_resource(get_root_resource()));

  auto gpu = std::make_unique<IntelDevice>(parent);
  if (!gpu)
    return ZX_ERR_NO_MEMORY;

  zx_status_t status = gpu->Init();
  if (status != ZX_OK) {
    return status;
  }
  // DdkAdd in Init took ownership of device.
  (void)gpu.release();

  DLOG("initialized magma system driver");

  return ZX_OK;
}

static constexpr zx_driver_ops_t msd_driver_ops = []() {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = sysdrv_bind;
  return ops;
}();

ZIRCON_DRIVER(gpu, msd_driver_ops, "magma", "0.1");
