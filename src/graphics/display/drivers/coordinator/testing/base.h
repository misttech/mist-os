// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_TESTING_BASE_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_TESTING_BASE_H_

#include <fidl/fuchsia.hardware.display/cpp/wire.h>
#include <fidl/fuchsia.sysmem2/cpp/wire.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/fit/function.h>
#include <lib/zx/bti.h>
#include <lib/zx/time.h>

#include <memory>

#include <gtest/gtest.h>

#include "src/graphics/display/drivers/fake/fake-display-stack.h"

namespace display_coordinator {

class TestBase : public testing::Test {
 public:
  TestBase();
  ~TestBase() override;

  void SetUp() override;
  void TearDown() override;

  // Guaranteed to be non-null. The Coordinator is guaranteed to be alive
  // throughout the entire test case, between SetUp() and TearDown().
  Controller* CoordinatorController();

  fake_display::FakeDisplay& FakeDisplayEngine();

  fidl::ClientEnd<fuchsia_sysmem2::Allocator> ConnectToSysmemAllocatorV2();
  const fidl::WireSyncClient<fuchsia_hardware_display::Provider>& DisplayProviderClient();

  async_dispatcher_t* dispatcher() { return loop_.dispatcher(); }

  // Waits until `predicate` returns true.
  //
  // `predicate` will only be evaluated on `loop_`.
  //
  // Returns true if the last evaluation of`predicate` returned true. Returns
  // false when the predicate can no longer be evaluated, such as when the
  // loop is destroyed.
  bool PollUntilOnLoop(fit::function<bool()> predicate, zx::duration poll_interval = zx::msec(10));

 private:
  async::Loop loop_;

  std::unique_ptr<display::FakeDisplayStack> fake_display_stack_;
};

}  // namespace display_coordinator

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_TESTING_BASE_H_
