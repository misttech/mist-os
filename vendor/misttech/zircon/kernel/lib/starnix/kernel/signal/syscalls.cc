// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/signals/syscalls.h"

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/task/current_task.h>
#include <lib/mistos/starnix/kernel/task/kernel.h>
#include <lib/mistos/starnix/kernel/task/process_group.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/mistos/starnix_uapi/user_address.h>
#include <lib/mistos/starnix_uapi/user_buffer.h>

#include <ktl/optional.h>

#include "../kernel_priv.h"

#include <ktl/enforce.h>

#define LOCAL_TRACE STARNIX_KERNEL_GLOBAL_TRACE(0)

using namespace starnix_uapi;

namespace starnix {

namespace {
// Negates the `pid` safely or fails with `ESRCH` (negation operation panics for `i32::MIN`).
fit::result<Errno, pid_t> negate_pid(pid_t pid) {
  if (pid == ktl::numeric_limits<pid_t>::min()) {
    return fit::error(errno(ESRCH));
  }
  return fit::ok(-pid);
}
}  // namespace

// Waits on the task with `pid` to exit or change state.
//
// - `current_task`: The current task.
// - `selector`: The selector for the task(s) to wait on.
// - `options`: The options passed to the wait syscall.
fit::result<Errno, ktl::optional<WaitResult>> wait_on_pid(const CurrentTask& current_task,
                                                          const ProcessSelector& selector,
                                                          const WaitingOptions& options) {
  // TODO: Implement Waiter class
  // auto waiter = Waiter::new();

  do {
    {
      auto pids = current_task->kernel()->pids.Write();

      // Waits and notifies on a given task need to be done atomically
      // with respect to changes to the task's waitable state; otherwise,
      // we see missing notifications. We do that by holding the task lock.
      // This next line checks for waitable traces without holding the
      // task lock, because constructing WaitResult objects requires
      // holding all sorts of locks that are incompatible with holding the
      // task lock.  We therefore have to check to see if a tracee has
      // become waitable again, after we acquire the lock.

      // TODO: Implement ptrace-related functionality
      // if (auto tracee = current_task.thread_group->get_waitable_ptracee(selector, options,
      // &pids)) {
      //   return fit::ok(ktl::make_optional(ktl::move(tracee)));
      //

      {
        auto thread_group = current_task->thread_group()->Write();

        // TODO: Implement ptrace-related functionality
        // bool has_waitable_tracee = false;
        // bool has_any_tracee = false;
        // current_task.thread_group->get_ptracees_and(
        //     selector,
        //     &pids,
        //     [&](WeakRef<Task> task, const TaskMutableState* task_state) {
        //       // Check for waitable tracees
        //     });

        // if (has_waitable_tracee || thread_group.zombie_ptracees.has_match(&selector, &pids)) {
        //   continue;
        // }

        auto result = thread_group->get_waitable_child(selector, options, *pids);
        switch (result.GetType()) {
          case WaitableChildResult::Type::ReadyNow:
            return fit::ok(result.GetResult());
          case WaitableChildResult::Type::ShouldWait:
            break;
          case WaitableChildResult::Type::NoneFound:
            // if !has_any_tracee {}
            return fit::error(errno(ECHILD));
        }
      }
    }

    if (!options.block()) {
      return fit::ok(ktl::nullopt);
    }

    // TODO: Implement waiting mechanism
    // waiter.wait();
    break;
  } while (true);

  // This return statement is unreachable but added to satisfy the compiler
  return fit::error(errno(EINTR));
}

fit::result<Errno, pid_t> sys_wait4(const CurrentTask& current_task, pid_t raw_selector,
                                    starnix_uapi::UserRef<int32_t> user_wstatus, uint32_t options,
                                    starnix_uapi::UserRef<struct ::rusage> user_rusage) {
  auto waiting_options =
      WaitingOptions::new_for_wait4(options, current_task->get_pid()) _EP(waiting_options);

  auto selector = [&]() -> fit::result<Errno, ProcessSelector> {
    if (raw_selector == 0) {
      return fit::ok(ProcessSelector::ProcessGroup(
          current_task->thread_group()->Read()->process_group->leader));
    } else if (raw_selector == -1) {
      return fit::ok(ProcessSelector::AnyProcess());
    } else if (raw_selector > 0) {
      return fit::ok(ProcessSelector::SpecificPid(raw_selector));
    } else if (raw_selector < -1) {
      auto negated = negate_pid(raw_selector) _EP(negated);
      return fit::ok(ProcessSelector::ProcessGroup(negated.value()));
    }
    // track_stub!(
    //       TODO("https://fxbug.dev/322874213"),
    //       "wait4 with selector",
    //       raw_selector as u64
    //  );
    // TODO(https://fxbug.dev/322874213): Implement wait4 with selector -1
    return fit::error(errno(ENOSYS));
  }() _EP(selector);

  auto waitable_process =
      wait_on_pid(current_task, selector.value(), waiting_options.value()) _EP(waitable_process);

  if (waitable_process.value().has_value()) {
    auto& process = waitable_process.value().value();
    int32_t status = ExitStatusHelper::wait_status(process.exit_info.status);

    if (!user_rusage->is_null()) {
      // TODO(https://fxbug.dev/322874768): Implement real rusage from wait4
      /*struct ::rusage usage = {
          .ru_utime = timeval_from_duration(process.time_stats.user_time),
          .ru_stime = timeval_from_duration(process.time_stats.system_time),
      };*/

      struct ::rusage usage = {};
      _EP(current_task->write_object(user_rusage, usage));
    }

    if (!user_wstatus->is_null()) {
      _EP(current_task.write_object(user_wstatus, status));
    }

    return fit::ok(process.pid);
  }

  return fit::ok(0);
}

}  // namespace starnix
