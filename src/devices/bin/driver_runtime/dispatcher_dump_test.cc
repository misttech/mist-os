// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/driver/runtime/testing/runtime/dispatcher.h>
#include <lib/fdf/cpp/dispatcher.h>
#include <lib/fdf/cpp/env.h>
#include <lib/fdf/testing.h>
#include <lib/sync/cpp/completion.h>

#include "src/devices/bin/driver_runtime/dispatcher.h"
#include "src/devices/bin/driver_runtime/runtime_test_case.h"

namespace driver_runtime {
extern DispatcherCoordinator& GetDispatcherCoordinator();
}

class DispatcherDumpTest : public RuntimeTestCase {
 public:
  void SetUp() override;
  void TearDown() override;

 protected:
  static constexpr std::string_view kDispatcherName = "synchronized";
  fdf::SynchronizedDispatcher dispatcher_;
  libsync::Completion shutdown_completion_;

  const void* fake_driver_ = nullptr;
};

void DispatcherDumpTest::SetUp() {
  driver_runtime::GetDispatcherCoordinator().Reset();

  fake_driver_ = CreateFakeDriver();
  auto shutdown_handler = [&](fdf_dispatcher_t* shutdown_dispatcher) {
    shutdown_completion_.Signal();
  };
  auto dispatcher = fdf::TestDispatcherBuilder::CreateTestingSynchronizedDispatcher(
      fake_driver_, {}, kDispatcherName, shutdown_handler);
  ASSERT_OK(dispatcher.status_value());
  dispatcher_ = std::move(*dispatcher);
}

void DispatcherDumpTest::TearDown() {
  dispatcher_.ShutdownAsync();
  ASSERT_OK(fdf_testing_run_until_idle());
  shutdown_completion_.Wait();
}

TEST_F(DispatcherDumpTest, DumpNoTasks) {
  {
    driver_runtime::Dispatcher::DumpState state;
    driver_runtime::Dispatcher* runtime_dispatcher =
        static_cast<driver_runtime::Dispatcher*>(dispatcher_.get());
    runtime_dispatcher->Dump(&state);

    ASSERT_EQ(state.running_dispatcher, nullptr);
    ASSERT_EQ(state.running_driver, nullptr);
    ASSERT_EQ(state.dispatcher_to_dump, dispatcher_.get());
    ASSERT_EQ(state.driver_owner, fake_driver_);
    ASSERT_EQ(state.state, driver_runtime::Dispatcher::DispatcherState::kRunning);
    ASSERT_EQ(state.name.data(), kDispatcherName);
    ASSERT_TRUE(state.synchronized);
    ASSERT_FALSE(state.allow_sync_calls);
    ASSERT_EQ(state.queued_tasks.size(), 0);
  }
}

TEST_F(DispatcherDumpTest, DumpFromTask) {
  // Queue 2 tasks, and dump the dispatcher state during each task.
  libsync::Completion task_completion;
  ASSERT_OK(async::PostTask(dispatcher_.async_dispatcher(), [&] {
    {
      driver_runtime::Dispatcher::DumpState state;
      driver_runtime::Dispatcher* runtime_dispatcher =
          static_cast<driver_runtime::Dispatcher*>(dispatcher_.get());
      runtime_dispatcher->Dump(&state);

      ASSERT_EQ(state.running_dispatcher, dispatcher_.get());
      ASSERT_EQ(state.running_driver, fake_driver_);
      ASSERT_EQ(state.dispatcher_to_dump, dispatcher_.get());
      ASSERT_EQ(state.driver_owner, fake_driver_);
      ASSERT_EQ(state.state, driver_runtime::Dispatcher::DispatcherState::kRunning);
      ASSERT_EQ(state.name.data(), kDispatcherName);
      ASSERT_EQ(state.queued_tasks.size(),
                1);  // We will post a second task before running the runtime thread.
    }
    task_completion.Signal();
  }));

  libsync::Completion task_completion2;
  ASSERT_OK(async::PostTask(dispatcher_.async_dispatcher(), [&] {
    {
      driver_runtime::Dispatcher::DumpState state;
      driver_runtime::Dispatcher* runtime_dispatcher =
          static_cast<driver_runtime::Dispatcher*>(dispatcher_.get());
      runtime_dispatcher->Dump(&state);

      ASSERT_EQ(state.running_dispatcher, dispatcher_.get());
      ASSERT_EQ(state.running_driver, fake_driver_);
      ASSERT_EQ(state.dispatcher_to_dump, dispatcher_.get());
      ASSERT_EQ(state.driver_owner, fake_driver_);
      ASSERT_EQ(state.state, driver_runtime::Dispatcher::DispatcherState::kRunning);
      ASSERT_EQ(state.name.data(), kDispatcherName);
      ASSERT_EQ(state.queued_tasks.size(), 0);  // This is the last task that was queued.
    }
    task_completion2.Signal();
  }));

  ASSERT_OK(fdf_testing_run_until_idle());
  task_completion.Wait();
  task_completion2.Wait();
}

TEST_F(DispatcherDumpTest, DumpFromAnotherDispatcher) {
  constexpr std::string_view kAdditionalDispatcherName = "additional_dispatcher";

  auto fake_driver2 = CreateFakeDriver();
  libsync::Completion shutdown_completion;
  auto shutdown_handler = [&](fdf_dispatcher_t* shutdown_dispatcher) {
    shutdown_completion.Signal();
  };
  auto dispatcher2 = fdf::TestDispatcherBuilder::CreateTestingSynchronizedDispatcher(
      fake_driver2, {}, kAdditionalDispatcherName, shutdown_handler);
  ASSERT_OK(dispatcher2.status_value());

  libsync::Completion task_completion;
  ASSERT_OK(async::PostTask(dispatcher_.async_dispatcher(), [&] {
    {
      driver_runtime::Dispatcher::DumpState state;
      driver_runtime::Dispatcher* runtime_dispatcher =
          static_cast<driver_runtime::Dispatcher*>(dispatcher2->get());
      runtime_dispatcher->Dump(&state);

      // The dispatcher being dumped is different from the dispatcher running on the current
      // thread.
      ASSERT_EQ(state.running_dispatcher, dispatcher_.get());
      ASSERT_EQ(state.running_driver, fake_driver_);
      ASSERT_EQ(state.dispatcher_to_dump, dispatcher2->get());
      ASSERT_EQ(state.driver_owner, fake_driver2);
      ASSERT_EQ(state.state, driver_runtime::Dispatcher::DispatcherState::kRunning);
      ASSERT_EQ(state.name.data(), kAdditionalDispatcherName);
      ASSERT_EQ(state.queued_tasks.size(), 0);
    }
    task_completion.Signal();
  }));

  ASSERT_OK(fdf_testing_run_until_idle());
  task_completion.Wait();

  dispatcher2->ShutdownAsync();
  ASSERT_OK(fdf_testing_run_until_idle());
  shutdown_completion.Wait();
}

TEST_F(DispatcherDumpTest, QueueTaskFromAnotherDispatcher) {
  constexpr std::string_view kAdditionalDispatcherName = "additional_dispatcher";

  auto fake_driver2 = CreateFakeDriver();
  libsync::Completion shutdown_completion;
  auto shutdown_handler = [&](fdf_dispatcher_t* shutdown_dispatcher) {
    shutdown_completion.Signal();
  };
  auto dispatcher2 = fdf::TestDispatcherBuilder::CreateTestingSynchronizedDispatcher(
      fake_driver2, {}, kAdditionalDispatcherName, shutdown_handler);
  ASSERT_OK(dispatcher2.status_value());

  libsync::Completion task_completion;
  libsync::Completion task_completion2;
  ASSERT_OK(async::PostTask(dispatcher_.async_dispatcher(), [&] {
    // Queue a task on the other dispatcher and print dump the details.
    ASSERT_OK(async::PostTask(dispatcher2->async_dispatcher(), [&] { task_completion2.Signal(); }));

    {
      driver_runtime::Dispatcher::DumpState state;
      driver_runtime::Dispatcher* runtime_dispatcher2 =
          static_cast<driver_runtime::Dispatcher*>(dispatcher2->get());
      runtime_dispatcher2->Dump(&state);

      // The dispatcher being dumped is different from the dispatcher running on the current
      // thread.
      ASSERT_EQ(state.running_dispatcher, dispatcher_.get());
      ASSERT_EQ(state.running_driver, fake_driver_);
      ASSERT_EQ(state.dispatcher_to_dump, dispatcher2->get());
      ASSERT_EQ(state.driver_owner, fake_driver2);
      ASSERT_EQ(state.state, driver_runtime::Dispatcher::DispatcherState::kRunning);
      ASSERT_EQ(state.name.data(), kAdditionalDispatcherName);
      ASSERT_EQ(state.queued_tasks.size(), 1);
      ASSERT_EQ(state.queued_tasks[0].initiating_dispatcher, dispatcher_.get());
      ASSERT_EQ(state.queued_tasks[0].initiating_driver, fake_driver_);
    }
    task_completion.Signal();
  }));

  ASSERT_OK(fdf_testing_run_until_idle());
  task_completion.Wait();
  task_completion2.Wait();

  dispatcher2->ShutdownAsync();
  ASSERT_OK(fdf_testing_run_until_idle());
  shutdown_completion.Wait();
}

// We will use the C API for the shutdown test, as async_task_t is not automatically dropped
// on cancellation like async::Task is.
struct Task {
  async_task_t task;
  libsync::Completion completion;
};

void TaskHandler(async_dispatcher_t* dispatcher, async_task_t* task, zx_status_t result) {
  auto* typed_task = reinterpret_cast<Task*>(task);
  typed_task->completion.Signal();
}

TEST_F(DispatcherDumpTest, DumpDuringShutdown) {
  Task task;
  task.task.state = ASYNC_STATE_INIT;
  task.task.handler = &TaskHandler;

  EXPECT_OK(async_post_task(dispatcher_.async_dispatcher(), &task.task));

  dispatcher_.ShutdownAsync();

  {
    driver_runtime::Dispatcher::DumpState state;
    driver_runtime::Dispatcher* runtime_dispatcher =
        static_cast<driver_runtime::Dispatcher*>(dispatcher_.get());
    runtime_dispatcher->Dump(&state);

    ASSERT_EQ(state.running_dispatcher, nullptr);
    ASSERT_EQ(state.running_driver, nullptr);
    ASSERT_EQ(state.dispatcher_to_dump, dispatcher_.get());
    ASSERT_EQ(state.driver_owner, fake_driver_);
    ASSERT_EQ(state.state, driver_runtime::Dispatcher::DispatcherState::kShuttingDown);
    ASSERT_EQ(state.name.data(), kDispatcherName);
    ASSERT_EQ(state.queued_tasks.size(), 1);
    ASSERT_EQ(state.queued_tasks[0].ptr, &task.task);
    ASSERT_EQ(state.queued_tasks[0].initiating_dispatcher, nullptr);
    ASSERT_EQ(state.queued_tasks[0].initiating_driver, nullptr);
  }

  ASSERT_OK(fdf_testing_run_until_idle());
  task.completion.Wait();
  shutdown_completion_.Wait();
}

TEST_F(DispatcherDumpTest, DumpUnsynchronizedDispatcher) {
  constexpr std::string_view kDispatcherName = "unsynchronized";

  auto fake_driver2 = CreateFakeDriver();
  libsync::Completion completion;
  auto shutdown_handler = [&](fdf_dispatcher_t* shutdown_dispatcher) { completion.Signal(); };
  auto dispatcher = fdf::TestDispatcherBuilder::CreateTestingUnsynchronizedDispatcher(
      fake_driver2, {}, kDispatcherName, shutdown_handler);

  libsync::Completion task_completion;
  ASSERT_OK(async::PostTask(dispatcher_.async_dispatcher(), [&] {
    {
      driver_runtime::Dispatcher::DumpState state;
      driver_runtime::Dispatcher* runtime_dispatcher =
          static_cast<driver_runtime::Dispatcher*>(dispatcher->get());

      runtime_dispatcher->Dump(&state);
      ASSERT_EQ(state.running_dispatcher, dispatcher_.get());
      ASSERT_EQ(state.running_driver, fake_driver_);
      ASSERT_EQ(state.dispatcher_to_dump, dispatcher->get());
      ASSERT_EQ(state.driver_owner, fake_driver2);
      ASSERT_EQ(state.state, driver_runtime::Dispatcher::DispatcherState::kRunning);
      ASSERT_EQ(state.name.data(), kDispatcherName);
      ASSERT_FALSE(state.synchronized);
      ASSERT_FALSE(state.allow_sync_calls);
      ASSERT_EQ(state.queued_tasks.size(), 0);
    }
    task_completion.Signal();
  }));

  ASSERT_OK(fdf_testing_run_until_idle());
  task_completion.Wait();

  dispatcher->ShutdownAsync();
  ASSERT_OK(fdf_testing_run_until_idle());
  completion.Wait();
}
