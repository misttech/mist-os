// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_TESTS_BASE_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_TESTS_BASE_H_

#include <fidl/fuchsia.hardware.display/cpp/wire.h>
#include <fuchsia/hardware/platform/device/c/banjo.h>
#include <fuchsia/hardware/platform/device/cpp/banjo.h>
#include <fuchsia/hardware/sysmem/c/banjo.h>
#include <fuchsia/hardware/sysmem/cpp/banjo.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/ddk/platform-defs.h>
#include <lib/zx/bti.h>
#include <threads.h>

#include <map>
#include <memory>
#include <vector>

#include <ddktl/device.h>
#include <fbl/array.h>
#include <gtest/gtest.h>

#include "src/graphics/display/drivers/fake/fake-display-stack.h"

namespace display {

class TestBase : public testing::Test {
 public:
  TestBase() : loop_(&kAsyncLoopConfigAttachToCurrentThread) {}

  void SetUp() override;
  void TearDown() override;

  Controller* controller() { return tree_->coordinator_controller(); }
  fake_display::FakeDisplay* display() { return tree_->display(); }

  const fidl::WireSyncClient<fuchsia_sysmem2::DriverConnector>& sysmem_fidl();
  const fidl::WireSyncClient<fuchsia_hardware_display::Provider>& display_fidl();

  async_dispatcher_t* dispatcher() { return loop_.dispatcher(); }
  bool RunLoopWithTimeoutOrUntil(fit::function<bool()>&& condition,
                                 zx::duration timeout = zx::sec(1),
                                 zx::duration step = zx::msec(10));

 private:
  async::Loop loop_;
  thrd_t loop_thrd_ = 0;

  std::unique_ptr<FakeDisplayStack> tree_;
};

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_TESTS_BASE_H_
