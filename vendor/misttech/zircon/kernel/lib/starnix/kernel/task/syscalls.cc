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
#include <lib/mistos/starnix/kernel/vfs/file_object.h>
#include <lib/mistos/starnix/kernel/vfs/path.h>
#include <lib/mistos/starnix_syscalls/syscall_result.h>
#include <lib/mistos/starnix_uapi/open_flags.h>
#include <lib/mistos/starnix_uapi/resource_limits.h>
#include <lib/mistos/starnix_uapi/signals.h>
#include <lib/mistos/starnix_uapi/user_address.h>
#include <lib/mistos/util/num.h>
#include <lib/mistos/util/weak_wrapper.h>
#include <trace.h>
#include <zircon/compiler.h>

#include <arch/mistos.h>
#include <fbl/alloc_checker.h>
#include <fbl/vector.h>
#include <ktl/algorithm.h>
#include <ktl/optional.h>
#include <object/process_dispatcher.h>

#include <linux/errno.h>
#include <linux/prctl.h>

using namespace starnix_syscalls;

namespace {

using starnix_uapi::UserAddress;
using starnix_uapi::UserCString;
using starnix_uapi::UserRef;

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

fit::result<Errno, pid_t> do_clone(CurrentTask& current_task, struct clone_args args) {
  // security::check_task_create_access(current_task)?;

  ktl::optional<Signal> child_exit_signal;
  if (args.exit_signal == 0) {
    child_exit_signal = {};
  } else {
    auto signal = Signal::try_from(UncheckedSignal::New(args.exit_signal)) _EP(signal);
    child_exit_signal = signal.value();
  }

  // Store the register state in the current task.
  ::zx_thread_state_general_regs_t regs;
  arch_get_general_regs_mistos(Thread::Current().Get(), &regs);
  current_task.thread_state.registers = RegisterState::From(regs);

  auto task_builder = current_task.clone_task(
      args.flags, child_exit_signal, UserRef<pid_t>::New(UserAddress::from((args.parent_tid))),
      UserRef<pid_t>::New(UserAddress::from((args.child_tid)))) _EP(task_builder);
  // Set the result register to 0 for the return value from clone in the
  // cloned process.
  auto new_task = task_builder.value();
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

fit::result<Errno, pid_t> sys_clone3(CurrentTask& current_task,
                                     starnix_uapi::UserRef<struct clone_args> user_clone_args,
                                     size_t user_clone_args_size) {
  // Only these specific sized versions are supported.
  if (user_clone_args_size != CLONE_ARGS_SIZE_VER0 &&
      user_clone_args_size != CLONE_ARGS_SIZE_VER1 &&
      user_clone_args_size != CLONE_ARGS_SIZE_VER2) {
    return fit::error(errno(EINVAL));
  }

  // The most recent version of the struct size should match our definition.
  static_assert(sizeof(struct clone_args) == CLONE_ARGS_SIZE_VER2);

  auto clone_args =
      current_task.read_object_partial(user_clone_args, user_clone_args_size) _EP(clone_args);
  return do_clone(current_task, clone_args.value());
}

fit::result<Errno> sys_execve(CurrentTask& current_task, UserCString user_path,
                              UserRef<starnix_uapi::UserCString> user_argv,
                              UserRef<starnix_uapi::UserCString> user_environ) {
  return sys_execveat(current_task, FdNumber::AT_FDCWD_, user_path, user_argv, user_environ, 0);
}

fit::result<Errno> sys_execveat(CurrentTask& current_task, FdNumber dir_fd, UserCString user_path,
                                UserRef<starnix_uapi::UserCString> user_argv,
                                UserRef<starnix_uapi::UserCString> user_environ, uint32_t flags) {
  if (flags & ~(AT_EMPTY_PATH | AT_SYMLINK_NOFOLLOW)) {
    return fit::error(errno(EINVAL));
  }

  // Calculate the limit for argv and environ size as 1/4 of the stack size, floored at 32 pages.
  // See the Limits sections in https://man7.org/linux/man-pages/man2/execve.2.html
  const size_t PAGE_LIMIT = 32;
  size_t page_limit_size = PAGE_LIMIT * static_cast<size_t>(PAGE_SIZE);
  auto rlimit =
      current_task->thread_group->get_rlimit(starnix_uapi::Resource{.value = ResourceEnum::STACK});
  auto stack_limit = rlimit / 4;
  auto argv_env_limit = ktl::max(page_limit_size, static_cast<size_t>(stack_limit));

  // The limit per argument or environment variable is 32 pages.
  // See the Limits sections in https://man7.org/linux/man-pages/man2/execve.2.html

  fit::result<Errno, ktl::pair<fbl::Vector<FsString>, size_t>> result_argv =
      user_argv->is_null() ? fit::ok(ktl::pair(ktl::move(fbl::Vector<FsString>()), 0))
                           : [&]() -> fit::result<Errno, ktl::pair<fbl::Vector<FsString>, size_t>> {
    auto result =
        read_c_string_vector(current_task, user_argv, page_limit_size, argv_env_limit) _EP(result);
    return result.take_value();
  }() _EP(result_argv);

  auto& [argv, argv_size] = result_argv.value();

  fit::result<Errno, ktl::pair<fbl::Vector<FsString>, size_t>> result_enpv =
      user_environ->is_null()
          ? fit::ok(ktl::pair(ktl::move(fbl::Vector<FsString>()), 0))
          : [&]() -> fit::result<Errno, ktl::pair<fbl::Vector<FsString>, size_t>> {
    auto result = read_c_string_vector(current_task, user_environ, page_limit_size,
                                       argv_env_limit - argv_size) _EP(result);
    return result.take_value();
  }() _EP(result_enpv);

  auto& [environ, _] = result_enpv.value();

  auto path =
      current_task->read_c_string_to_vec(user_path, static_cast<size_t>(PATH_MAX)) _EP(path);

  TRACEF("execveat(%d, %.*s)", dir_fd.raw(), static_cast<int>(path->size()), path->data());

  starnix_uapi::OpenFlags open_flags = starnix_uapi::OpenFlags(starnix_uapi::OpenFlagsEnum::RDONLY);
  if ((flags & AT_SYMLINK_NOFOLLOW) != 0) {
    open_flags |= starnix_uapi::OpenFlagsEnum::NOFOLLOW;
  }

  auto executable = [&]() -> fit::result<Errno, FileHandle> {
    if (path->empty()) {
      if ((flags & AT_EMPTY_PATH) == 0) {
        // If AT_EMPTY_PATH is not set, this is an error.
        return fit::error(errno(ENOENT));
      }

      // O_PATH allowed for:
      //
      //   Passing the file descriptor as the dirfd argument of
      //   openat() and the other "*at()" system calls.  This
      //   includes linkat(2) with AT_EMPTY_PATH (or via procfs
      //   using AT_SYMLINK_FOLLOW) even if the file is not a
      //   directory.
      //
      // See https://man7.org/linux/man-pages/man2/open.2.html
      auto file = current_task->files.get_allowing_opath(dir_fd) _EP(file);

      // We are forced to reopen the file with O_RDONLY to get access to the underlying VMO.
      // Note that skip the access check in the arguments in case the file mode does
      // not actually have the read permission bit.
      //
      // This can happen because a file could have --x--x--x mode permissions and then
      // be opened with O_PATH. Internally, the file operations would all be stubbed out
      // for that file, which is undesirable here.
      //
      // See https://man7.org/linux/man-pages/man3/fexecve.3.html#DESCRIPTION
      // file->name()
      return file->name.open(current_task,
                             starnix_uapi::OpenFlags(starnix_uapi::OpenFlagsEnum::RDONLY),
                             /* AccessCheck::check_for(Access::EXEC)*/ true);
    } else {
      return current_task.open_file_at(
          dir_fd, *path, open_flags, FileMode(),
          ResolveFlags::empty() /*, AccessCheck::check_for(Access::EXEC)*/);
    }
  }() _EP(executable);

  // This path can affect script resolution (the path is appended to the script args)
  // and the auxiliary value `AT_EXECFN` from the syscall `getauxval()`
  auto npath = [&]() {
    if (dir_fd == FdNumber::AT_FDCWD_) {
      // The file descriptor is CWD, so the path is exactly
      // what the user specified.
      return path.value();
    } else {
      // The path is `/dev/fd/N/P` where N is the file descriptor
      // number and P is the user-provided path (if relative and non-empty).
      //
      // See https://man7.org/linux/man-pages/man2/execveat.2.html#NOTES
      if (path->empty()) {
        // User-provided path is empty
        // None => format!("/dev/fd/{}", dir_fd.raw()).into_bytes(),
      }
      switch (path.value()[0]) {
        case '/': {
          // The user-provided path is absolute, so dir_fd is ignored.
          return path.value();
        }
        default: {
          // User-provided path is relative, append it.
          // let mut new_path = format!("/dev/fd/{}/", dir_fd.raw()).into_bytes();
          // new_path.append(&mut path.to_vec());
          // new_path
        }
      }
      return BString();
    }
  }();

  _EP(current_task.exec(*executable, npath, argv, environ));
  return fit::ok();
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
