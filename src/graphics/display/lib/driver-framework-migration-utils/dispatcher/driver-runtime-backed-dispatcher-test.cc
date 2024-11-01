// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/async/cpp/irq.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/testing/cpp/driver_runtime.h>
#include <lib/driver/testing/cpp/internal/driver_lifecycle.h>
#include <lib/driver/testing/cpp/internal/test_environment.h>
#include <lib/driver/testing/cpp/test_node.h>
#include <lib/fpromise/promise.h>
#include <lib/sync/cpp/completion.h>
#include <lib/zx/interrupt.h>
#include <zircon/errors.h>
#include <zircon/rights.h>
#include <zircon/status.h>
#include <zircon/types.h>

#include <gtest/gtest.h>

#include "src/graphics/display/lib/driver-framework-migration-utils/dispatcher/testing/dfv2-driver-with-dispatcher.h"
#include "src/lib/testing/predicates/status.h"

namespace display {

namespace {

// WARNING: Don't use this test as a template for new tests as it uses the old driver testing
// library.
// Tests dispatching asynchronous tasks and IRQ handler events on the
// `fdf::Dispatcher`-backed dispatcher.
//
// Note that this test doesn't test the functionality of setting the scheduler
// role for dispatcher threads. This is because `fdf::Dispatcher` always
// connects to the fucshia.scheduler.RoleManager protocol in the **component's**
// incoming service directory to set the scheduler role. The only way to test
// it is by creating a realm-manager-based integration test, which we haven't
// implemented yet.

class DriverDispatcherTest : public ::testing::Test {
 public:
  void SetUp() override {
    // Create start args
    node_server_.emplace("root");
    zx::result start_args = node_server_->CreateStartArgsAndServe();
    EXPECT_OK(start_args);

    // Start the test environment
    test_environment_.emplace();
    zx::result result =
        test_environment_->Initialize(std::move(start_args->incoming_directory_server));
    EXPECT_OK(result);

    // Start driver
    zx::result start_result =
        runtime_.RunToCompletion(driver_.Start(std::move(start_args->start_args)));
    EXPECT_OK(start_result);
  }

  void TearDown() override {
    StopDriver();

    test_environment_.reset();
    node_server_.reset();
  }

  // Stops the driver, shuts down its all dispatchers and returns true, if the
  // driver is not yet stopped. Otherwise returns false.
  //
  // Must be called only from the main test thread.
  bool StopDriver() {
    if (driver_stopped_) {
      return false;
    }
    zx::result prepare_stop_result = runtime_.RunToCompletion(driver_.PrepareStop());
    EXPECT_OK(prepare_stop_result);
    runtime_.ShutdownAllDispatchers(fdf::Dispatcher::GetCurrent()->get());
    driver_stopped_ = true;
    return true;
  }

 protected:
  // Attaches a foreground dispatcher for us automatically.
  fdf_testing::DriverRuntime runtime_;

  // These will use the foreground dispatcher.
  std::optional<fdf_testing::TestNode> node_server_;
  std::optional<fdf_testing::internal::TestEnvironment> test_environment_;
  fdf_testing::internal::DriverUnderTest<testing::Dfv2DriverWithDispatcher> driver_;

  bool driver_stopped_ = false;
};

TEST_F(DriverDispatcherTest, DispatchAsyncTask) {
  fpromise::bridge<uint32_t> bridge;
  static constexpr uint32_t kValueToPass = 0xabcd1234;
  zx::result<> post_task_result = driver_->PostTask(
      [completer = std::move(bridge.completer)]() mutable { completer.complete_ok(kValueToPass); });
  ASSERT_OK(post_task_result.status_value());

  fpromise::promise<uint32_t> promise = std::move(bridge.consumer).promise();
  fpromise::result<uint32_t> promise_result = runtime_.RunPromise(std::move(promise));
  ASSERT_TRUE(promise_result.is_ok());
  EXPECT_EQ(promise_result.value(), kValueToPass);

  ASSERT_TRUE(StopDriver());

  // After the driver stops, no task can be posted to the driver's async
  // dispatcher.
  zx::result<> post_task_after_driver_stop_result = driver_->PostTask(
      [completer = std::move(bridge.completer)]() mutable { completer.complete_ok(0x1234abcd); });
  EXPECT_NE(ZX_OK, post_task_after_driver_stop_result.status_value());
}

TEST_F(DriverDispatcherTest, HandleIrq) {
  zx::interrupt virtual_interrupt;
  zx_status_t status =
      zx::interrupt::create(zx::resource{}, 0u, ZX_INTERRUPT_VIRTUAL, &virtual_interrupt);
  ASSERT_OK(status);

  zx::interrupt virtual_interrupt_driver_dup;
  status = virtual_interrupt.duplicate(ZX_RIGHT_SAME_RIGHTS, &virtual_interrupt_driver_dup);
  ASSERT_OK(status);

  zx::time_boot latest_handled_irq_timestamp;
  libsync::Completion irq_handler_invoked;
  libsync::Completion irq_handler_canceled;

  async::Irq::Handler handler = [&latest_handled_irq_timestamp, &irq_handler_invoked,
                                 &irq_handler_canceled](async_dispatcher_t* dispatcher,
                                                        async::Irq* irq, zx_status_t status,
                                                        const zx_packet_interrupt_t* interrupt) {
    ASSERT_TRUE(status == ZX_OK || status == ZX_ERR_CANCELED)
        << "Invalid async Irq wait status: " << zx_status_get_string(status);
    if (status == ZX_ERR_CANCELED) {
      irq_handler_canceled.Signal();
      return;
    }
    latest_handled_irq_timestamp = zx::time_boot(interrupt->timestamp);
    irq_handler_invoked.Signal();

    // Acknowledges the interrupt so that it can be triggered again.
    zx::unowned_interrupt(irq->object())->ack();
  };

  zx::result<> start_irq_handler_result =
      driver_->StartIrqHandler(std::move(virtual_interrupt_driver_dup), std::move(handler));
  ASSERT_OK(start_irq_handler_result.status_value());

  // Manually trigger the virtual interrupt.
  static constexpr zx::time_boot kIrqTimestamp1 = zx::time_boot(0x12345678);
  status = virtual_interrupt.trigger(0u, kIrqTimestamp1);
  ASSERT_OK(status);

  // The interrupt handler is invoked when the interrupt is triggered.
  irq_handler_invoked.Wait();
  EXPECT_EQ(latest_handled_irq_timestamp, kIrqTimestamp1);

  // Manually trigger the virtual interrupt again.
  irq_handler_invoked.Reset();
  static constexpr zx::time_boot kIrqTimestamp2 = zx::time_boot(0x23456789);
  status = virtual_interrupt.trigger(0u, kIrqTimestamp2);
  ASSERT_OK(status);

  // The interrupt handler can be invoked again when the interrupt is triggered
  // again.
  irq_handler_invoked.Wait();
  EXPECT_EQ(latest_handled_irq_timestamp, kIrqTimestamp2);

  // Stop the driver and its dispatchers. The handler should receive a
  // ZX_ERR_CANCELED signal.
  ASSERT_TRUE(StopDriver());
  irq_handler_canceled.Wait();
}

}  // namespace

}  // namespace display
