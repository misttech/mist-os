// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <zircon/status.h>
#include <zircon/syscalls.h>

#include <memory>

#include <fbl/unique_fd.h>
#include <gtest/gtest.h>

#include "integration-test.h"

namespace libdriver_integration_test {

class BasicLifecycleTest : public IntegrationTest {};

// This test checks what happens when a driver returns an error from bind.
TEST_F(BasicLifecycleTest, BindError) {
  std::unique_ptr<RootMockDevice> root_mock_device;

  auto promise =
      ExpectBind(&root_mock_device, [](HookInvocation record, Completer<void> completer) {
        completer.complete_ok();
        ActionList actions;
        actions.AppendReturnStatus(ZX_ERR_NOT_SUPPORTED);
        return actions;
      });
  RunPromise(std::move(promise));
}

// This test confirms that after a device has been added:
// 1) When it's parent is removed, the device receives its unbind() callback.
// 2) If the device calls device_remove() in the unbind() callback, its
//    release() callback gets called later.
TEST_F(BasicLifecycleTest, BindThenUnbindAndRemove) {
  std::unique_ptr<RootMockDevice> root_mock_device;
  std::unique_ptr<MockDevice> mock_child_device;

  constexpr char kName[] = "sys/test/test/mock/first_child";

  auto promise = CreateFirstChild(&root_mock_device, &mock_child_device)
                     .and_then(DoWaitForPath(kName))
                     .and_then([&]() -> Promise<void> {
                       // Destroy the test device.  This should cause an unbind of the child
                       // device.
                       root_mock_device.reset();
                       return ExpectUnbindThenRelease(mock_child_device);
                     });

  RunPromise(std::move(promise));
}

}  // namespace libdriver_integration_test
