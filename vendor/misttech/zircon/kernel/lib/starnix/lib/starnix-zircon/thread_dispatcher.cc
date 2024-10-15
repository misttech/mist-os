// Copyright 2024 Mist Tecnologia LTDA
// Copyright 2016 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "object/thread_dispatcher.h"

#include <lib/starnix_zircon/task_wrapper.h>
#include <trace.h>

#include <ktl/move.h>

#define LOCAL_TRACE 0

void ThreadDispatcher::SetTask(fbl::RefPtr<TaskWrapper> task) { starnix_task_ = ktl::move(task); }

// Get the associated Starnix Task.
TaskWrapper* ThreadDispatcher::task() { return starnix_task_.get(); }
const TaskWrapper* ThreadDispatcher::task() const { return starnix_task_.get(); }

zx_status_t ThreadDispatcher::SetForkFrame(const zx_thread_state_general_regs_t& fork_frame) {
  canary_.Assert();

  LTRACE_ENTRY_OBJ;

  Guard<CriticalMutex> guard{get_lock()};

  switch (state_.lifecycle()) {
    case ThreadState::Lifecycle::INITIAL:
      // Unreachable, thread leaves INITIAL state before Create() returns.
      DEBUG_ASSERT(false);
      __UNREACHABLE;
    case ThreadState::Lifecycle::INITIALIZED:
      return core_thread_->SetForkFrame(fork_frame);
    case ThreadState::Lifecycle::RUNNING:
    case ThreadState::Lifecycle::SUSPENDED:
    case ThreadState::Lifecycle::DYING:
    case ThreadState::Lifecycle::DEAD:
      return ZX_ERR_BAD_STATE;
  }

  DEBUG_ASSERT(false);
  return ZX_ERR_BAD_STATE;
}
