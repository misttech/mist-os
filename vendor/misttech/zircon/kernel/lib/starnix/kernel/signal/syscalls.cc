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
#include <lib/mistos/starnix/kernel/task/waiter.h>
#include <lib/mistos/starnix/kernel/vfs/file_object.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/mistos/starnix_uapi/user_address.h>
#include <lib/mistos/starnix_uapi/user_buffer.h>

#include <ktl/optional.h>

#include "../kernel_priv.h"

#include <ktl/enforce.h>

#define LOCAL_TRACE STARNIX_KERNEL_GLOBAL_TRACE(0)

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

fit::result<Errno> sys_kill(const CurrentTask& current_task, pid_t pid,
                            starnix_uapi::UncheckedSignal unchecked_signal) {
  auto pids = current_task->kernel()->pids.Write();

  if (pid > 0) {
    // "If pid is positive, then signal sig is sent to the process with
    // the ID specified by pid."
    auto target_thread_group = [&]() -> fit::result<Errno, fbl::RefPtr<ThreadGroup>> {
      if (auto process = pids->get_process(pid)) {
        return ktl::visit(
            ProcessEntryRef::overloaded{
                [](const fbl::RefPtr<ThreadGroup>& thread_group)
                    -> fit::result<Errno, fbl::RefPtr<ThreadGroup>> {
                  return fit::ok(thread_group);
                },

                // Zombies cannot receive signals. Just ignore it.
                [](const fbl::RefPtr<ZombieProcess>&)
                    -> fit::result<Errno, fbl::RefPtr<ThreadGroup>> { return fit::ok(nullptr); }},

            process.value().variant_);
      }

      // If we don't have process with `pid` then check if there is a task with
      // the `pid`.
      auto weak_task = pids->get_task(pid);
      auto task = weak_task.Lock();
      if (task) {
        return fit::ok(task->thread_group_);
      }
      return fit::error(errno(ESRCH));
    }() _EP(target_thread_group);

    if (auto thread_group = target_thread_group.value()) {
      // return thread_group->send_signal_unchecked(current_task, unchecked_signal);
    }
  } else if (pid == -1) {
    // "If pid equals -1, then sig is sent to every process for which
    // the calling process has permission to send signals, except for
    // process 1 (init), but ... POSIX.1-2001 requires that kill(-1,sig)
    // send sig to all processes that the calling process may send
    // signals to, except possibly for some implementation-defined
    // system processes. Linux allows a process to signal itself, but on
    // Linux the call kill(-1,sig) does not signal the calling process."

    auto thread_groups = pids->get_thread_groups();
    for (const auto& thread_group : thread_groups) {
      if (thread_group == current_task->thread_group_ || thread_group->leader_ == 1) {
        continue;
      }
      // auto result = thread_group->send_signal_unchecked(current_task, unchecked_signal)
      // _EP(result);
    }
  } else {
    // "If pid equals 0, then sig is sent to every process in the
    // process group of the calling process."
    //
    // "If pid is less than -1, then sig is sent to every process in the
    // process group whose ID is -pid."

    pid_t process_group_id;
    if (pid == 0) {
      process_group_id = current_task->thread_group_->Read()->process_group_->leader_;
    } else {
      auto negated = negate_pid(pid) _EP(negated);
      process_group_id = negated.value();
    }

    if (auto process_group = pids->get_process_group(process_group_id)) {
      auto thread_groups = (*process_group)->Read()->thread_groups();
      for (auto thread_group : thread_groups) {
        // auto result = thread_group->send_signal_unchecked(current_task, unchecked_signal)
        // _EP(result);
      }
    }
  }
  return fit::ok();
}

namespace {
/// Waits on the task with `pid` to exit or change state.
///
/// - `current_task`: The current task.
/// - `pid`: The id of the task to wait on.
/// - `options`: The options passed to the wait syscall.
fit::result<Errno, ktl::optional<WaitResult>> wait_on_pid(const CurrentTask& current_task,
                                                          const ProcessSelector& selector,
                                                          const WaitingOptions& options) {
  auto waiter = Waiter::New();
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
        auto thread_group = current_task->thread_group_->Write();

        // TODO (Herrera): Implement ptrace-related functionality
        // Per the above, see if traced tasks have become waitable. If they have, release
        // the lock and retry getting waitable tracees.
        // bool has_waitable_tracee = false;
        bool has_any_tracee = false;
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
            if (!has_any_tracee) {
              return fit::error(errno(ECHILD));
            }
            break;
        }
        thread_group->child_status_waiters_.wait_async(waiter);
      }
    }

    if (!options.block()) {
      return fit::ok(ktl::nullopt);
    }

    _EP(map_eintr(waiter.wait(current_task), errno(ERESTARTSYS)));
  } while (true);
}
}  // namespace

fit::result<Errno> sys_waitid(const CurrentTask& current_task, uint32_t id_type, int32_t id,
                              starnix_uapi::UserAddress user_info, uint32_t options,
                              starnix_uapi::UserRef<struct ::rusage> user_rusage) {
  auto waiting_options = WaitingOptions::new_for_waitid(options) _EP(waiting_options);

  auto task_selector = [&]() -> fit::result<Errno, ProcessSelector> {
    switch (id_type) {
      case P_PID:
        return fit::ok(ProcessSelector::SpecificPid(id));
      case P_ALL:
        return fit::ok(ProcessSelector::AnyProcess());
      case P_PGID:
        if (id == 0) {
          return fit::ok(ProcessSelector::ProcessGroup(
              current_task->thread_group_->Read()->process_group_->leader_));
        }
        return fit::ok(ProcessSelector::ProcessGroup(id));
      case P_PIDFD: {
        auto fd = FdNumber::from_raw(id);
        auto file = current_task->files_.get(fd) _EP(file);
        if (file->flags().contains(OpenFlagsEnum::NONBLOCK)) {
          // waiting_options.value().set_block(false);
        }
        auto pid = file->as_pid() _EP(pid);
        return fit::ok(ProcessSelector::SpecificPid(pid.value()));
      }
      default:
        return fit::error(errno(EINVAL));
    }
  }() _EP(task_selector);

  // wait_on_pid returns None if the task was not waited on. In that case, we don't write out a
  // siginfo. This seems weird but is the correct behavior according to the waitid(2) man page.
  auto waitable_process = wait_on_pid(current_task, task_selector.value(), waiting_options.value())
      _EP(waitable_process);

  if (waitable_process.value().has_value()) {
    auto& process = waitable_process.value().value();

    if (!user_rusage->is_null()) {
      // TODO(https://fxbug.dev/322874712): Implement real rusage from waitid
      struct ::rusage usage = {};
      _EP(current_task->write_object(user_rusage, usage));
    }

    if (!user_info.is_null()) {
      auto siginfo = process.as_signal_info();
      _EP(current_task->write_memory(user_info, siginfo.as_siginfo_bytes()));
    }
  } else if (id_type == P_PIDFD) {
    // From <https://man7.org/linux/man-pages/man2/pidfd_open.2.html>:
    //
    //   PIDFD_NONBLOCK
    //     Return a nonblocking file descriptor.  If the process
    //     referred to by the file descriptor has not yet terminated,
    //     then an attempt to wait on the file descriptor using
    //     waitid(2) will immediately return the error EAGAIN rather
    //     than blocking.
    return fit::error(errno(EAGAIN));
  }

  return fit::ok();
}

fit::result<Errno, pid_t> sys_wait4(const CurrentTask& current_task, pid_t raw_selector,
                                    starnix_uapi::UserRef<int32_t> user_wstatus, uint32_t options,
                                    starnix_uapi::UserRef<struct ::rusage> user_rusage) {
  auto waiting_options =
      WaitingOptions::new_for_wait4(options, current_task->get_pid()) _EP(waiting_options);

  auto selector = [&]() -> fit::result<Errno, ProcessSelector> {
    if (raw_selector == 0) {
      return fit::ok(ProcessSelector::ProcessGroup(
          current_task->thread_group_->Read()->process_group_->leader_));
    }
    if (raw_selector == -1) {
      return fit::ok(ProcessSelector::AnyProcess());
    }
    if (raw_selector > 0) {
      return fit::ok(ProcessSelector::SpecificPid(raw_selector));
    }
    if (raw_selector < -1) {
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

  {
    auto waitable_process =
        wait_on_pid(current_task, selector.value(), waiting_options.value()) _EP(waitable_process);

    if (waitable_process.value().has_value()) {
      auto& process = waitable_process.value().value();
      int32_t status = process.exit_info.status.wait_status();

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
}

fit::result<Errno, ktl::optional<WaitResult>> friend_wait_on_pid(const CurrentTask& current_task,
                                                                 const ProcessSelector& selector,
                                                                 const WaitingOptions& options) {
  return wait_on_pid(current_task, selector, options);
}

}  // namespace starnix
