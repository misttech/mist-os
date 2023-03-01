// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fuchsia/device/cpp/fidl.h>
#include <fuchsia/device/test/cpp/fidl.h>
#include <lib/ddk/platform-defs.h>
#include <zircon/status.h>

#include <memory>

#include <gtest/gtest.h>

#include "integration-test.h"

namespace libdriver_integration_test {

class CompositeDeviceTest : public IntegrationTest {
 public:
  static void SetUpTestSuite() { DoSetup(); }

 protected:
  // Create the fragments for the well-known composite that the mock sysdev creates.
  static Promise<void> CreateFragmentDevices(std::unique_ptr<RootMockDevice>* root_device,
                                             std::unique_ptr<MockDevice>* child1_device,
                                             std::unique_ptr<MockDevice>* child2_device) {
    fpromise::bridge<void, Error> child1_bridge;
    fpromise::bridge<void, Error> child2_bridge;
    return ExpectBind(root_device,
                      [=, child1_completer = std::move(child1_bridge.completer),
                       child2_completer = std::move(child2_bridge.completer)](
                          HookInvocation record, Completer<void> completer) mutable {
                        std::vector<zx_device_prop_t> child1_props({
                            {BIND_PLATFORM_DEV_VID, 0, PDEV_VID_TEST},
                            {BIND_PLATFORM_DEV_PID, 0, PDEV_PID_LIBDRIVER_TEST},
                            {BIND_PLATFORM_DEV_DID, 0, PDEV_DID_TEST_CHILD_1},
                        });
                        std::vector<zx_device_prop_t> child2_props({
                            {BIND_PLATFORM_DEV_VID, 0, PDEV_VID_TEST},
                            {BIND_PLATFORM_DEV_PID, 0, PDEV_PID_LIBDRIVER_TEST},
                            {BIND_PLATFORM_DEV_DID, 0, PDEV_DID_TEST_CHILD_2},
                        });
                        ActionList actions;
                        actions.AppendAddMockDevice(loop_.dispatcher(), (*root_device)->path(),
                                                    "fragment1", std::move(child1_props), ZX_OK,
                                                    std::move(child1_completer), child1_device);
                        actions.AppendAddMockDevice(loop_.dispatcher(), (*root_device)->path(),
                                                    "fragment2", std::move(child2_props), ZX_OK,
                                                    std::move(child2_completer), child2_device);
                        actions.AppendReturnStatus(ZX_OK);

                        (*child1_device)->set_hooks(std::make_unique<IgnoreGetProtocol>());
                        (*child2_device)->set_hooks(std::make_unique<IgnoreGetProtocol>());
                        completer.complete_ok();
                        return actions;
                      })
        .and_then(child1_bridge.consumer.promise_or(fpromise::error("child1 create abandoned")))
        .and_then(child2_bridge.consumer.promise_or(fpromise::error("child2 create abandoned")));
  }
};

// This test creates two devices that match the well-known composite in the
// test sysdev driver.  It then waits for it to appear in devfs.
TEST_F(CompositeDeviceTest, CreateTest) {
  std::unique_ptr<RootMockDevice> root_device;
  std::unique_ptr<MockDevice> child_device1, child_device2;
  fidl::InterfacePtr<fuchsia::io::Node> client;

  constexpr char kName[] = "sys/test/test/mock/fragment1/composite";

  auto promise = CreateFragmentDevices(&root_device, &child_device1, &child_device2)
                     .and_then(DoWaitForPath(kName))
                     .and_then([&]() { return DoOpen(kName, &client); })
                     .and_then([&]() -> Promise<void> {
                       // Destroy the test device.  This should cause an unbind of the child
                       // device.
                       root_device.reset();
                       return JoinPromises(ExpectUnbindThenRelease(child_device1),
                                           ExpectUnbindThenRelease(child_device2));
                     });

  RunPromise(std::move(promise));
}

// This test creates the well-known composite, and force binds a test driver
// stack to the composite.  It then forces one of the fragments to unbind.
// It verifies that the composite mock-device's unbind hook is called.
TEST_F(CompositeDeviceTest, UnbindFragment) {
  std::unique_ptr<RootMockDevice> root_device, composite_mock;
  std::unique_ptr<MockDevice> child_device1, child_device2, composite_child_device;
  fidl::InterfacePtr<fuchsia::io::Node> client;
  fidl::InterfacePtr<fuchsia::device::Controller> child1_controller;

  constexpr char kName[] = "sys/test/test/mock/fragment1/composite/test";

  auto promise =
      CreateFragmentDevices(&root_device, &child_device1, &child_device2)
          .and_then(DoWaitForPath(kName))
          .and_then([&]() { return DoOpen(kName, &client); })
          .and_then([&]() -> Promise<void> {
            fidl::SynchronousInterfacePtr<fuchsia::device::test::RootDevice> composite_test;
            composite_test.Bind(client.Unbind().TakeChannel());

            auto bind_callback = [&composite_mock, &composite_child_device](
                                     HookInvocation record, Completer<void> completer) {
              // Create a test child that we can monitor for hooks.
              ActionList actions;
              actions.AppendAddMockDevice(loop_.dispatcher(), composite_mock->path(), "child",
                                          std::vector<zx_device_prop_t>{}, ZX_OK,
                                          std::move(completer), &composite_child_device);
              actions.AppendReturnStatus(ZX_OK);
              return actions;
            };

            fpromise::bridge<void, Error> bridge;
            auto bind_hook =
                std::make_unique<BindOnce>(std::move(bridge.completer), std::move(bind_callback));
            // Bind the mock device driver to a new child
            zx_status_t status = RootMockDevice::CreateFromTestRoot(
                devmgr_, loop_.dispatcher(), std::move(composite_test), std::move(bind_hook),
                &composite_mock);
            PROMISE_ASSERT(ASSERT_EQ(status, ZX_OK));

            return bridge.consumer.promise_or(fpromise::error("bind abandoned"));
          })
          .and_then([&]() -> Promise<void> {
            // Open up child1, so we can send it an unbind request
            return DoOpen(child_device1->path(), &client);
          })
          .and_then([&]() -> Promise<void> {
            zx_status_t status =
                child1_controller.Bind(client.Unbind().TakeChannel(), loop_.dispatcher());
            PROMISE_ASSERT(ASSERT_EQ(status, ZX_OK));

            // Setup our expectations for unbinding. The child device will be unbound,
            // then the composite will be unbound and released, then the child device will be
            // released.
            auto unbind_promise =
                ExpectUnbind(child_device1,
                             [](HookInvocation record, Completer<void> completer) {
                               ActionList actions;
                               Promise<void> unbind_reply_done;
                               actions.AppendUnbindReply(&unbind_reply_done);
                               completer.complete_ok();
                               return actions;
                             })
                    .and_then(ExpectUnbindThenRelease(composite_child_device))
                    .and_then(ExpectRelease(child_device1));

            // Send the unbind request to child1
            fpromise::bridge<void, Error> bridge;
            child1_controller->ScheduleUnbind(
                [completer = std::move(bridge.completer)](
                    fuchsia::device::Controller_ScheduleUnbind_Result result) mutable {
                  if (result.is_response()) {
                    completer.complete_ok();
                  } else {
                    completer.complete_error(std::string("unbind failed"));
                  }
                });

            return unbind_promise.and_then(
                bridge.consumer.promise_or(fpromise::error("Unbind abandoned")));
          })
          .and_then([&]() -> Promise<void> {
            // Destroy the test device.  This should cause an unbind of the last child
            // device.
            root_device.reset();
            return ExpectUnbindThenRelease(child_device2);
          });

  RunPromise(std::move(promise));
}

}  // namespace libdriver_integration_test
