// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/async/cpp/irq.h>
#include <lib/async/cpp/wait.h>
#include <lib/fdf/cpp/dispatcher.h>
#include <lib/fdf/cpp/env.h>
#include <lib/sync/cpp/completion.h>
#include <lib/zx/interrupt.h>

#include "src/devices/bin/driver_runtime/dispatcher.h"
#include "src/devices/bin/driver_runtime/driver_context.h"
#include "src/devices/bin/driver_runtime/runtime_test_case.h"

namespace driver_runtime {
extern DispatcherCoordinator& GetDispatcherCoordinator();
}

class ThreadPoolTest : public RuntimeTestCase {
 public:
  void SetUp() override;
  void TearDown() override;

  void InitTestDispatcher(std::string_view scheduler_role,
                          fdf::SynchronizedDispatcher::Options options = {});

  driver_runtime::Dispatcher* runtime_dispatcher() {
    return static_cast<driver_runtime::Dispatcher*>(dispatcher_.get());
  }

 protected:
  fdf::SynchronizedDispatcher dispatcher_;
  libsync::Completion shutdown_completion_;
};

void ThreadPoolTest::SetUp() {
  // Make sure each test starts with exactly one thread.
  driver_runtime::GetDispatcherCoordinator().Reset();
  ASSERT_EQ(ZX_OK, driver_runtime::GetDispatcherCoordinator().Start());
}

void ThreadPoolTest::TearDown() {
  if (dispatcher_.get() != nullptr) {
    dispatcher_.ShutdownAsync();
    shutdown_completion_.Wait();
  }
}

void ThreadPoolTest::InitTestDispatcher(std::string_view scheduler_role,
                                        fdf::SynchronizedDispatcher::Options options) {
  ZX_ASSERT(dispatcher_.get() == nullptr);

  auto fake_driver = CreateFakeDriver();
  auto shutdown_handler = [&](fdf_dispatcher_t* dispatcher) { shutdown_completion_.Signal(); };
  auto dispatcher = fdf_env::DispatcherBuilder::CreateSynchronizedWithOwner(
      fake_driver, options, "dispatcher", shutdown_handler, scheduler_role);
  ASSERT_FALSE(dispatcher.is_error());
  dispatcher_ = *std::move(dispatcher);
}

TEST_F(ThreadPoolTest, NoSchedulerRole) {
  constexpr std::string_view scheduler_role = "";
  ASSERT_NO_FATAL_FAILURE(InitTestDispatcher(scheduler_role));

  libsync::Completion task_completion;
  ASSERT_OK(async::PostTask(dispatcher_.async_dispatcher(), [&] {
    auto thread_pool = runtime_dispatcher()->thread_pool();

    ASSERT_EQ(thread_pool->scheduler_role(),
              driver_runtime::Dispatcher::ThreadPool::kNoSchedulerRole);
    ASSERT_TRUE(thread_pool->applied_scheduler_role());

    task_completion.Signal();
  }));
  task_completion.Wait();
}

TEST_F(ThreadPoolTest, WithSchedulerRole) {
  constexpr std::string_view scheduler_role = "fuchsia.test-role:ok";
  ASSERT_NO_FATAL_FAILURE(InitTestDispatcher(scheduler_role));

  libsync::Completion task_completion;
  ASSERT_OK(async::PostTask(dispatcher_.async_dispatcher(), [&] {
    auto thread_pool = runtime_dispatcher()->thread_pool();

    ASSERT_EQ(thread_pool->scheduler_role(), scheduler_role);
    ASSERT_TRUE(thread_pool->applied_scheduler_role());

    task_completion.Signal();
  }));
  task_completion.Wait();
}

TEST_F(ThreadPoolTest, BadSchedulerRole) {
  constexpr std::string_view scheduler_role = "fuchsia.test-role:not-found";
  ASSERT_NO_FATAL_FAILURE(InitTestDispatcher(scheduler_role));

  libsync::Completion task_completion;
  ASSERT_OK(async::PostTask(dispatcher_.async_dispatcher(), [&] {
    auto thread_pool = runtime_dispatcher()->thread_pool();

    ASSERT_EQ(thread_pool->scheduler_role(), scheduler_role);
    ASSERT_FALSE(thread_pool->applied_scheduler_role());

    task_completion.Signal();
  }));
  task_completion.Wait();
}

TEST_F(ThreadPoolTest, AllowSyncCalls) {
  constexpr std::string_view scheduler_role = "fuchsia.test-role:ok";
  ASSERT_NO_FATAL_FAILURE(
      InitTestDispatcher(scheduler_role, fdf::SynchronizedDispatcher::Options::kAllowSyncCalls));

  libsync::Completion task_completion;
  ASSERT_OK(async::PostTask(dispatcher_.async_dispatcher(), [&] {
    auto thread_pool = runtime_dispatcher()->thread_pool();

    ASSERT_EQ(thread_pool->scheduler_role(), scheduler_role);
    ASSERT_TRUE(thread_pool->applied_scheduler_role());
    ASSERT_EQ(thread_pool->num_threads(), 1u);

    ASSERT_EQ(driver_runtime::GetDispatcherCoordinator().default_thread_pool()->num_threads(), 1u);

    task_completion.Signal();
  }));
  task_completion.Wait();
}

TEST_F(ThreadPoolTest, Shutdown) {
  constexpr std::string_view scheduler_role = "fuchsia.test-role:ok";

  auto fake_driver = CreateFakeDriver();
  libsync::Completion shutdown_completion;
  auto shutdown_handler = [&](fdf_dispatcher_t* dispatcher) {
    driver_runtime::Dispatcher* runtime_dispatcher =
        static_cast<driver_runtime::Dispatcher*>(dispatcher);
    auto thread_pool = runtime_dispatcher->thread_pool();

    ASSERT_EQ(thread_pool->scheduler_role(), scheduler_role);
    ASSERT_TRUE(thread_pool->applied_scheduler_role());

    shutdown_completion.Signal();
  };
  auto dispatcher = fdf_env::DispatcherBuilder::CreateSynchronizedWithOwner(
      fake_driver, {}, "dispatcher", shutdown_handler, scheduler_role);
  ASSERT_FALSE(dispatcher.is_error());

  dispatcher->ShutdownAsync();
  shutdown_completion.Wait();
}
