// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/fake/sysmem-proxy-device.h"

#include <fidl/fuchsia.sysmem/cpp/wire.h>
#include <fidl/fuchsia.sysmem2/cpp/wire.h>
#include <lib/async-loop/loop.h>
#include <lib/async/cpp/task.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/driver.h>
#include <lib/fdio/directory.h>
#include <lib/zx/channel.h>
#include <zircon/assert.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <cstdint>
#include <utility>

#include <ddktl/device.h>
#include <ddktl/unbind-txn.h>

namespace display {

// severity can be ERROR, WARN, INFO, DEBUG, TRACE.  See ddk/debug.h.
//
// Using ## __VA_ARGS__ instead of __VA_OPT__(,) __VA_ARGS__ for now, since
// __VA_OPT__ doesn't seem to be available yet.
#define LOG(severity, fmt, ...) \
  zxlogf(severity, "[%s:%s:%d] " fmt "\n", "display", __func__, __LINE__, ##__VA_ARGS__)

SysmemProxyDevice::SysmemProxyDevice(zx_device_t* parent_device,
                                     sysmem_driver::Driver* parent_driver)
    : DdkDeviceType2(parent_device),
      parent_driver_(parent_driver),
      loop_(&kAsyncLoopConfigNeverAttachToThread) {
  ZX_DEBUG_ASSERT(parent_);
  ZX_DEBUG_ASSERT(parent_driver_);
  zx_status_t status = loop_.StartThread("sysmem", &loop_thrd_);
  ZX_ASSERT(status == ZX_OK);
}

void SysmemProxyDevice::ConnectV1(ConnectV1RequestView request,
                                  ConnectV1Completer::Sync& completer) {
  static constexpr char kServicePath[] = "/svc/fuchsia.sysmem.Allocator";
  LOG(INFO, "fdio_service_connect to service service: %s", kServicePath);
  zx_status_t status =
      fdio_service_connect(kServicePath, request->allocator_request.TakeChannel().release());
  if (status != ZX_OK) {
    LOG(INFO, "SysmemConnect() failed");
    return;
  }
}

void SysmemProxyDevice::ConnectV2(ConnectV2RequestView request,
                                  ConnectV2Completer::Sync& completer) {
  static constexpr char kServicePath[] = "/svc/fuchsia.sysmem2.Allocator";
  LOG(INFO, "fdio_service_connect to service service: %s", kServicePath);
  zx_status_t status =
      fdio_service_connect(kServicePath, request->allocator_request.TakeChannel().release());
  if (status != ZX_OK) {
    LOG(INFO, "SysmemConnect() failed");
    return;
  }
}

void SysmemProxyDevice::SetAuxServiceDirectory(SetAuxServiceDirectoryRequestView request,
                                               SetAuxServiceDirectoryCompleter::Sync& completer) {
  LOG(INFO, "SysmemProxyDevice::SetAuxServiceDirectory() not supported");
}

zx_status_t SysmemProxyDevice::Bind() {
  zx_status_t status = DdkAdd(ddk::DeviceAddArgs("sysmem")
                                  .set_flags(DEVICE_ADD_ALLOW_MULTI_COMPOSITE)
                                  .set_inspect_vmo(inspector_.DuplicateVmo()));
  if (status != ZX_OK) {
    LOG(ERROR, "Failed to bind device");
    return status;
  }

  return ZX_OK;
}

void SysmemProxyDevice::DdkUnbind(ddk::UnbindTxn txn) {
  // Ensure all tasks started before this call finish before shutting down the loop.
  async::PostTask(loop_.dispatcher(), [this]() { loop_.Quit(); });
  // JoinThreads waits for the Quit() to execute and cause the thread to exit.
  loop_.JoinThreads();
  loop_.Shutdown();
  // After this point the FIDL servers should have been shutdown and all DDK and other protocol
  // methods will error out because posting tasks to the dispatcher fails.
  txn.Reply();
}

}  // namespace display
