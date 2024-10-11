// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/task/syscalls.h"

#include <lib/fit/result.h>
#include <lib/mistos/linux_uapi/arch/x86_64.h>
#include <lib/mistos/starnix/kernel/mm/memory_manager.h>
#include <lib/mistos/starnix/kernel/task/current_task.h>
#include <lib/mistos/starnix/kernel/task/process_group.h>
#include <lib/mistos/starnix/kernel/task/session.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>
#include <lib/mistos/starnix/kernel/vfs/path.h>
#include <lib/mistos/starnix_syscalls/syscall_result.h>
#include <lib/mistos/starnix_uapi/signals.h>
#include <lib/mistos/starnix_uapi/user_address.h>
#include <lib/mistos/util/weak_wrapper.h>
#include <trace.h>
#include <zircon/compiler.h>

#include <ktl/algorithm.h>
#include <ktl/optional.h>
#include <object/process_dispatcher.h>

#include <linux/errno.h>
#include <linux/prctl.h>

using namespace starnix_uapi;
using namespace starnix_syscalls;

namespace {

util::WeakPtr<starnix::Task> get_task_or_current(const starnix::CurrentTask& current_task,
                                                 pid_t pid) {
  if (pid == 0) {
    return current_task.weak_task();
  } else {
    // TODO(security): Should this use get_task_if_owner_or_has_capabilities() ?
    return current_task->get_task(pid);
  }
}

void __NO_RETURN do_exit(long code) { ProcessDispatcher::ExitCurrent(code); }

}  // namespace

namespace starnix {

fit::result<Errno, pid_t> do_clone(const CurrentTask& current_task, struct clone_args args) {
  // security::check_task_create_access(current_task)?;

  ktl::optional<Signal> child_exit_signal;
  if (args.exit_signal == 0) {
    child_exit_signal = {};
  } else {
    auto signal_or_error = Signal::try_from(UncheckedSignal::New(args.exit_signal));
    if (signal_or_error.is_error())
      return signal_or_error.take_error();
    child_exit_signal = signal_or_error.value();
  }

  ///
  // READ TASK Registers

  ///

  auto new_task_or_error = current_task.clone_task(
      args.flags, child_exit_signal, UserRef<pid_t>::New(UserAddress::from((args.parent_tid))),
      UserRef<pid_t>::New(UserAddress::from((args.child_tid))));
  if (new_task_or_error.is_error()) {
    return new_task_or_error.take_error();
  }
  auto new_task = new_task_or_error.value();
  // Set the result register to 0 for the return value from clone in the
  // cloned process.
  new_task.thread_state.registers.set_return_register(0);
  // let (trace_kind, ptrace_state) = current_task.get_ptrace_core_state_for_clone(args);

  if (args.stack != 0) {
    // In clone() the `stack` argument points to the top of the stack, while in clone3()
    // `stack` points to the bottom of the stack. Therefore, in clone3() we need to add
    // `stack_size` to calculate the stack pointer. Note that in clone() `stack_size` is 0.
    new_task.thread_state.registers.set_stack_pointer_register(args.stack + args.stack_size);
  }

  if ((args.flags & static_cast<uint64_t>(CLONE_SETTLS)) != 0) {
    new_task.thread_state.registers.set_thread_pointer_register(args.tls);
  }

  auto tid = new_task.task->id;
  auto task_ref = util::WeakPtr(new_task.task.get());
  // execute_task(new_task, | _, _ | Ok(()), | _ | {}, ptrace_state);

  if ((args.flags & static_cast<uint64_t>(CLONE_VFORK)) != 0) {
    // current_task.wait_for_execve(task_ref) ? ;
    // current_task.ptrace_event(PtraceOptions::TRACEVFORKDONE, tid as u64);
  }

  return fit::ok(tid);
}

fit::result<Errno, ktl::pair<fbl::Vector<FsString>, size_t>> read_c_string_vector(
    const CurrentTask& mm, starnix_uapi::UserRef<starnix_uapi::UserCString> user_vector,
    size_t elem_limit, size_t vec_limit) {
  auto user_current = user_vector;
  fbl::AllocChecker ac;
  fbl::Vector<FsString> vector;
  size_t vec_size = 0;

  do {
    auto user_string = mm.read_object(user_current) _EP(user_string);
    if (user_string->is_null()) {
      break;
    }
    auto string = mm.read_c_string_to_vec(user_string.value(), elem_limit);
    if (string.is_error()) {
      auto e = string.error_value();
      if (e == errno(ENAMETOOLONG)) {
        return fit::error(errno(E2BIG));
      } else {
        return fit::error(e);
      }
    }
    // Plus one to consider the null terminated
    auto add_value = mtl::checked_add(vec_size, string->size() + 1);
    if (add_value.has_value()) {
      vec_size = add_value.value();
      if (vec_size > vec_limit) {
        return fit::error(errno(E2BIG));
      }
    } else {
      return fit::error(errno(E2BIG));
    }
    vector.push_back(ktl::move(string.value()), &ac);
    if (!ac.check()) {
      return fit::error(errno(ENOMEM));
    }
    user_current = user_current.next();
  } while (true);

  return fit::ok(ktl::pair(ktl::move(vector), vec_size));
}

fit::result<Errno, pid_t> sys_getpid(const CurrentTask& current_task) {
  return fit::ok(current_task->get_pid());
}

fit::result<Errno, pid_t> sys_gettid(const CurrentTask& current_task) {
  return fit::ok(current_task->get_tid());
}
fit::result<Errno, pid_t> sys_getppid(const CurrentTask& current_task) {
  return fit::ok(current_task->thread_group->read()->get_ppid());
}

fit::result<Errno, pid_t> sys_getsid(const CurrentTask& current_task, pid_t pid) {
  util::WeakPtr<Task> weak = get_task_or_current(current_task, pid);
  auto result = Task::from_weak(weak);
  if (result.is_error())
    return result.take_error();
  auto target_task = result.value();
  // security::check_task_getsid(current_task, &target_task)?;
  auto sid = target_task->thread_group->read()->process_group->session->leader;
  return fit::ok(sid);
}

fit::result<Errno, pid_t> sys_getpgid(const CurrentTask& current_task, pid_t pid) {
  util::WeakPtr<Task> weak = get_task_or_current(current_task, pid);
  auto result = Task::from_weak(weak);
  if (result.is_error())
    return result.take_error();

  auto task = result.value();
  // selinux_hooks::check_getpgid_access(current_task, &task)?;
  auto pgid = task->thread_group->read()->process_group->leader;
  return fit::ok(pgid);
}

fit::result<Errno, uid_t> sys_getuid(const CurrentTask& current_task) {
  return fit::ok(current_task->creds().uid);
}

fit::result<Errno, uid_t> sys_getgid(const CurrentTask& current_task) {
  return fit::ok(current_task->creds().gid);
}

fit::result<Errno, uid_t> sys_geteuid(const CurrentTask& current_task) {
  return fit::ok(current_task->creds().euid);
}

fit::result<Errno, uid_t> sys_getegid(const CurrentTask& current_task) {
  return fit::ok(current_task->creds().egid);
}

fit::result<Errno> sys_exit(const CurrentTask& current_task, uint32_t code) {
  // Only change the current exit status if this has not been already set by exit_group, as
  // otherwise it has priority.
  // current_task.write().set_exit_status_if_not_already(ExitStatus::Exit(code as u8));
  // Ok(())
  do_exit((code & 0xff) << 8);
  __UNREACHABLE;
}

fit::result<Errno> sys_exit_group(CurrentTask& current_task, uint32_t code) {
  current_task.thread_group_exit(ExitStatusExit(static_cast<uint8_t>(code)));
  do_exit((code & 0xff) << 8);
  __UNREACHABLE;
}

fit::result<Errno, SyscallResult> sys_prctl(const CurrentTask& current_task, int option,
                                            uint64_t arg2, uint64_t arg3, uint64_t arg4,
                                            uint64_t arg5) {
  switch (option) {
    case PR_SET_VMA: {
      if (arg2 != PR_SET_VMA_ANON_NAME) {
        // track_stub !(TODO("https://fxbug.dev/322874826"), "prctl PR_SET_VMA", arg2);
        return fit::error(errno(ENOSYS));
      }
      auto addr = UserAddress::from(arg3);
      auto length = static_cast<size_t>(arg4);
      auto name_addr = UserAddress::from(arg5);

      auto name = ktl::optional<FsString>();
      if (!name_addr.is_null()) {
        auto uname = UserCString::New(UserAddress::from(arg5));
        auto name_or_error = current_task.read_c_string_to_vec(uname, 256).map_error([](Errno e) {
          // An overly long name produces EINVAL and not ENAMETOOLONG in Linux 5.15.
          if (e == errno(ENAMETOOLONG)) {
            return errno(EINVAL);
          } else {
            return e;
          }
        });
        if (name_or_error.is_error())
          return name_or_error.take_error();

        auto fname = name_or_error.value();
        if (ktl::any_of(fname.begin(), fname.end(), [](uint8_t b) {
              return (b <= 0x1f) || (b >= 0x7f && b <= 0xff) || (b == '\\') || (b == '`') ||
                     (b == '$') || (b == '[') || (b == ']');
            })) {
          return fit::error(errno(EINVAL));
        }
        name = fname;
      }
      auto result = current_task->mm()->set_mapping_name(addr, length, name);
      if (result.is_error())
        return result.take_error();
      return fit::ok(starnix_syscalls::SUCCESS);
    }
    // case PR_SET_NAME:
    // auto addr = UserAddress::from(arg2);
    default:
      return fit::error(errno(ENOSYS));
  }
}

}  // namespace starnix
