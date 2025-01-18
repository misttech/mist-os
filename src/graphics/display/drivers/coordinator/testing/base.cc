// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/coordinator/testing/base.h"

#include <fidl/fuchsia.hardware.sysmem/cpp/fidl.h>
#include <lib/async-loop/default.h>
#include <lib/async/cpp/task.h>
#include <lib/driver/logging/cpp/logger.h>
#include <zircon/compiler.h>
#include <zircon/status.h>

#include <memory>
#include <optional>

#include <fbl/auto_lock.h>
#include <fbl/condition_variable.h>
#include <fbl/mutex.h>

#include "src/graphics/display/drivers/coordinator/post-display-task.h"
#include "src/graphics/display/drivers/fake/fake-display.h"
#include "src/graphics/display/drivers/fake/fake-sysmem-device-hierarchy.h"
#include "src/lib/testing/predicates/status.h"

namespace display_coordinator {

TestBase::TestBase() : loop_(&kAsyncLoopConfigNeverAttachToThread) {}

TestBase::~TestBase() = default;

void TestBase::SetUp() {
  loop_.StartThread("display::TestBase::loop_");

  zx::result<std::unique_ptr<fake_display::FakeSysmemDeviceHierarchy>>
      create_sysmem_provider_result = fake_display::FakeSysmemDeviceHierarchy::Create();
  ASSERT_OK(create_sysmem_provider_result);

  static constexpr fake_display::FakeDisplayDeviceConfig kDeviceConfig = {
      .periodic_vsync = false,
      .no_buffer_access = false,
  };
  fake_display_stack_ = std::make_unique<fake_display::FakeDisplayStack>(
      std::move(create_sysmem_provider_result).value(), kDeviceConfig);
}

void TestBase::TearDown() {
  fake_display_stack_->SyncShutdown();
  async::PostTask(loop_.dispatcher(), [this]() { loop_.Quit(); });

  // Wait for loop_.Quit() to execute.
  loop_.JoinThreads();
}

bool TestBase::PollUntilOnLoop(fit::function<bool()> predicate, zx::duration poll_interval) {
  struct PredicateEvalState {
    // Immutable after construction.
    fit::function<bool()> predicate __TA_GUARDED(mutex);

    fbl::Mutex mutex;

    // Signaled after `result` has a value.
    fbl::ConditionVariable signal;

    // nullopt while waiting for the predicate to be evaluated.
    std::optional<bool> result __TA_GUARDED(mutex);
  };
  PredicateEvalState predicate_eval_state = {
      .predicate = std::move(predicate),
  };

  while (true) {
    auto post_task_state = std::make_unique<DisplayTaskState>();

    {
      fbl::AutoLock lock(&predicate_eval_state.mutex);
      predicate_eval_state.result = std::nullopt;
    }

    // Retaining `predicate_eval_state` by reference is safe because this method
    // will block on a ConditionVariable::Wait() call, which is only fired at
    // the end of the task handler.
    zx::result post_task_result = display::PostTask(
        std::move(post_task_state), *loop_.dispatcher(), [&predicate_eval_state]() {
          fbl::AutoLock lock(&predicate_eval_state.mutex);

          predicate_eval_state.result = predicate_eval_state.predicate();

          // Once `result` is set to true above, PollUntilOnLoop() may return as
          // soon as it acquires `mutex`. So, `predicate_eval_state` can only be
          // used while `mutex` remains held.
          //
          // This means that we must call ConditionVariable::Signal() while
          // holding `mutex`.
          predicate_eval_state.signal.Signal();
        });
    if (post_task_result.is_error()) {
      FDF_LOG(ERROR, "Failed to post task: %s", post_task_result.status_string());
      return false;
    }

    {
      fbl::AutoLock lock(&predicate_eval_state.mutex);
      while (!predicate_eval_state.result.has_value()) {
        predicate_eval_state.signal.Wait(&predicate_eval_state.mutex);
      }
      if (predicate_eval_state.result.value()) {
        return true;
      }
    }

    zx_status_t sleep_status = zx::nanosleep(zx::deadline_after(poll_interval));
    if (sleep_status != ZX_OK) {
      FDF_LOG(ERROR, "Failed to sleep: %s", zx_status_get_string(sleep_status));
      return false;
    }
  }
}

Controller* TestBase::CoordinatorController() {
  return fake_display_stack_->coordinator_controller();
}

fake_display::FakeDisplay& TestBase::FakeDisplayEngine() {
  return fake_display_stack_->display_engine();
}

fidl::ClientEnd<fuchsia_sysmem2::Allocator> TestBase::ConnectToSysmemAllocatorV2() {
  return fake_display_stack_->ConnectToSysmemAllocatorV2();
}
const fidl::WireSyncClient<fuchsia_hardware_display::Provider>& TestBase::DisplayProviderClient() {
  return fake_display_stack_->display_provider_client();
}

}  // namespace display_coordinator
