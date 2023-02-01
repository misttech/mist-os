// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/async/cpp/task.h>
#include <lib/fit/defer.h>
#include <lib/sync/cpp/completion.h>
#include <lib/zx/channel.h>

#include <gtest/gtest.h>

#include "mock/mock_msd_cc.h"
#include "sys_driver_cpp/magma_system_connection.h"
#include "sys_driver_cpp/magma_system_device.h"

namespace {

class TestMsdConnection : public MsdMockConnection {
 public:
  ~TestMsdConnection() override { *connection_torn_down_ = true; }

  void SetNotificationCallback(msd::NotificationHandler* handler) override { handler_ = handler; }

  void SendTask() {
    auto deferred_teardown = fit::defer_callback(
        [connection_torn_down = connection_torn_down_, completion = completion_]() {
          // Check that the connection is still in existence at the time this code is run.
          EXPECT_FALSE(*connection_torn_down);
          completion->Signal();
        });

    // The task should either be run or canceled before the msd::Connection is
    // destroyed. In either case, the task's destructor will be run, executing
    // the defer_callback function above.
    async::PostTask(handler_->GetAsyncDispatcher(),
                    [deferred_teardown = std::move(deferred_teardown)]() mutable {});
  }

  std::shared_ptr<libsync::Completion> completion() { return completion_; }

 private:
  msd::NotificationHandler* handler_ = nullptr;
  std::shared_ptr<bool> connection_torn_down_ = std::make_shared<bool>(false);
  std::shared_ptr<libsync::Completion> completion_ = std::make_shared<libsync::Completion>();
};

class TestMsdDevice : public MsdMockDevice {
 public:
  explicit TestMsdDevice(std::unique_ptr<msd::Connection> connection)
      : connection_(std::move(connection)) {}
  std::unique_ptr<msd::Connection> Open(msd_client_id_t client_id) override {
    return std::move(connection_);
  }

 private:
  std::unique_ptr<msd::Connection> connection_;
};

// Test that callbacks are shutdown before tearing down the connection when using
// MagmaSystemDevice::Shutdown.
TEST(MagmaNotification, NotAfterShutdown) {
  auto msd_driver = std::make_unique<MsdMockDriver>();
  auto msd_connection = std::make_unique<TestMsdConnection>();
  auto connection_ptr = msd_connection.get();
  auto msd_dev = std::make_unique<TestMsdDevice>(std::move(msd_connection));
  auto dev = std::shared_ptr<MagmaSystemDevice>(
      MagmaSystemDevice::Create(msd_driver.get(), std::move(msd_dev)));

  zx::channel server_fidl, client_fidl;
  ASSERT_EQ(ZX_OK, zx::channel::create(0, &server_fidl, &client_fidl));
  zx::channel server_notification, client_notification;
  ASSERT_EQ(ZX_OK, zx::channel::create(0, &server_notification, &client_notification));
  auto zircon_connection =
      MagmaSystemDevice::Open(dev, 0, magma::PlatformHandle::Create(std::move(server_fidl)),
                              magma::PlatformHandle::Create(std::move(server_notification)));

  dev->StartConnectionThread(std::move(zircon_connection), nullptr);
  auto completion = connection_ptr->completion();
  connection_ptr->SendTask();

  dev->Shutdown();
  completion->Wait();
}

// Test that callbacks are shutdown before tearing down the connection when the connection is
// closed.
TEST(MagmaNotification, NotAfterConnectionTeardown) {
  auto msd_driver = std::make_unique<MsdMockDriver>();
  auto msd_connection = std::make_unique<TestMsdConnection>();
  auto connection_ptr = msd_connection.get();
  auto msd_dev = std::make_unique<TestMsdDevice>(std::move(msd_connection));
  auto dev = std::shared_ptr<MagmaSystemDevice>(
      MagmaSystemDevice::Create(msd_driver.get(), std::move(msd_dev)));

  zx::channel server_fidl, client_fidl;
  ASSERT_EQ(ZX_OK, zx::channel::create(0, &server_fidl, &client_fidl));
  zx::channel server_notification, client_notification;
  ASSERT_EQ(ZX_OK, zx::channel::create(0, &server_notification, &client_notification));
  auto zircon_connection =
      MagmaSystemDevice::Open(dev, 0, magma::PlatformHandle::Create(std::move(server_fidl)),
                              magma::PlatformHandle::Create(std::move(server_notification)));

  dev->StartConnectionThread(std::move(zircon_connection), nullptr);
  auto completion = connection_ptr->completion();
  connection_ptr->SendTask();

  // Should trigger the client connection to close.
  client_fidl.reset();
  completion->Wait();

  dev->Shutdown();
}

}  // namespace
