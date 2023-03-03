// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_SYSMEM_TESTS_SYSMEM_FUZZ_SYSMEM_FUZZ_COMMON_H_
#define SRC_DEVICES_SYSMEM_TESTS_SYSMEM_FUZZ_SYSMEM_FUZZ_COMMON_H_

#include <fidl/fuchsia.sysmem/cpp/wire.h>

#include "src/devices/sysmem/drivers/sysmem/device.h"
#include "src/devices/sysmem/drivers/sysmem/driver.h"
#include "src/devices/testing/mock-ddk/mock-device.h"

class MockDdkSysmem {
 public:
  ~MockDdkSysmem();
  std::shared_ptr<MockDevice>& root() { return root_; }

  bool Init();
  zx::result<fidl::ClientEnd<fuchsia_sysmem::Allocator>> Connect();

 protected:
  bool initialized_ = false;
  std::shared_ptr<MockDevice> root_ = MockDevice::FakeRootParent();

  sysmem_driver::Driver sysmem_ctx_;
  sysmem_driver::Device sysmem_{root_.get(), &sysmem_ctx_};
  async::Loop loop_{&kAsyncLoopConfigNeverAttachToThread};
};

#endif  // SRC_DEVICES_SYSMEM_TESTS_SYSMEM_FUZZ_SYSMEM_FUZZ_COMMON_H_
