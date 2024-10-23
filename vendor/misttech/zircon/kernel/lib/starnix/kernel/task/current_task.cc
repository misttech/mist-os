// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/task/current_task.h"

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/arch/x64/registers.h>
#include <lib/mistos/starnix/kernel/execution/executor.h>
#include <lib/mistos/starnix/kernel/loader.h>
#include <lib/mistos/starnix/kernel/mm/memory_manager.h>
#include <lib/mistos/starnix/kernel/task/kernel.h>
#include <lib/mistos/starnix/kernel/task/process_group.h>
#include <lib/mistos/starnix/kernel/task/session.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>
#include <lib/mistos/starnix/kernel/vfs/fd_number.h>
#include <lib/mistos/starnix/kernel/vfs/file_object.h>
#include <lib/mistos/starnix/kernel/vfs/fs_context.h>
#include <lib/mistos/starnix/kernel/vfs/fs_node.h>
#include <lib/mistos/starnix/kernel/vfs/lookup_context.h>
#include <lib/mistos/starnix/kernel/vfs/namespace.h>
#include <lib/mistos/starnix/kernel/vfs/symlink_mode.h>
#include <lib/mistos/starnix/testing/testing.h>
#include <lib/mistos/starnix_uapi/auth.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/mistos/starnix_uapi/file_mode.h>
#include <lib/mistos/starnix_uapi/signals.h>
#include <lib/mistos/starnix_uapi/user_address.h>
#include <lib/mistos/util/bstring.h>
#include <lib/mistos/util/default_construct.h>
#include <lib/mistos/util/strings/split_string.h>
#include <lib/mistos/util/weak_wrapper.h>
#include <lib/starnix_sync/locks.h>
#include <lib/user_copy/user_ptr.h>
#include <trace.h>
#include <zircon/assert.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <utility>

#include <fbl/alloc_checker.h>
#include <fbl/ref_ptr.h>
#include <kernel/mutex.h>
#include <ktl/string_view.h>
#include <lockdep/guard.h>
#include <object/handle.h>
#include <object/process_dispatcher.h>
#include <object/thread_dispatcher.h>

#include "../kernel_priv.h"

#include <ktl/enforce.h>

#include <linux/resource.h>
#include <linux/sched.h>

#define LOCAL_TRACE STARNIX_KERNEL_GLOBAL_TRACE(0)

using namespace util;

namespace starnix {

TaskBuilder::TaskBuilder(fbl::RefPtr<Task> task) : task_(ktl::move(task)) {}

TaskBuilder::TaskBuilder(TaskBuilder&& other) {
  task_ = ktl::move(other.task_);
  thread_state_ = ktl::move(other.thread_state_);
}

TaskBuilder& TaskBuilder::operator=(TaskBuilder&& other) {
  task_ = ktl::move(other.task_);
  thread_state_ = ktl::move(other.thread_state_);
  return *this;
}

TaskBuilder::~TaskBuilder() = default;

const Task* TaskBuilder::operator->() const {
  ASSERT_MSG(task_, "called `operator->` empty Task");
  return task_.get();
}

Task* TaskBuilder::operator->() {
  ASSERT_MSG(task_, "called `operator->` empty Task");
  return task_.get();
}

CurrentTask::CurrentTask(fbl::RefPtr<Task> task, ThreadState thread_state)
    : task_(ktl::move(task)), thread_state_(thread_state) {}

CurrentTask::CurrentTask(CurrentTask&& other) {
  task_ = ktl::move(other.task_);
  thread_state_ = ktl::move(other.thread_state_);
}

CurrentTask& CurrentTask::operator=(CurrentTask&& other) {
  task_ = ktl::move(other.task_);
  thread_state_ = ktl::move(other.thread_state_);
  return *this;
}

CurrentTask::~CurrentTask() { LTRACE_ENTRY_OBJ; }

Task* CurrentTask::operator->() {
  ASSERT_MSG(task_, "called `operator->()` empty Task");
  return task_.get();
}

const Task* CurrentTask::operator->() const {
  ASSERT_MSG(task_, "called `operator->()` empty Task");
  return task_.get();
}

Task& CurrentTask::operator*() {
  ASSERT_MSG(task_, "called `operator*()` empty Task");
  return *task_;
}

const Task& CurrentTask::operator*() const {
  ASSERT_MSG(task_, "called `operator*()` empty Task");
  return *task_;
}

fit::result<Errno, TaskBuilder> CurrentTask::create_init_process(
    const fbl::RefPtr<Kernel>& kernel, pid_t pid, const ktl::string_view& initial_name,
    fbl::RefPtr<FsContext> fs, fbl::Array<ktl::pair<starnix_uapi::Resource, uint64_t>> rlimits) {
  LTRACE;
  auto pids = kernel->pids.Write();

  auto task_info_factory = [kernel, initial_name](pid_t pid,
                                                  fbl::RefPtr<ProcessGroup> process_group) {
    return create_zircon_process(kernel, {}, pid, process_group, initial_name);
  };

  return create_task_with_pid(kernel, pids, pid, initial_name, fs, task_info_factory,
                              Credentials::root(), ktl::move(rlimits));
}

fit::result<Errno, CurrentTask> CurrentTask::create_system_task(const fbl::RefPtr<Kernel>& kernel,
                                                                fbl::RefPtr<FsContext> fs) {
  auto builder = CurrentTask::create_task(
      kernel, "[kthreadd]", fs,
      [kernel](pid_t pid, fbl::RefPtr<ProcessGroup> process_group) -> fit::result<Errno, TaskInfo> {
        KernelHandle<ProcessDispatcher> process;
        auto memory_manager = fbl::RefPtr(MemoryManager::new_empty());
        auto thread_group = ThreadGroup::New(kernel, ktl::move(process), {}, pid, process_group);

        return fit::ok(
            TaskInfo{.thread = {}, .thread_group = thread_group, .memory_manager = memory_manager});
      }) _EP(builder);

  return fit::ok(starnix::CurrentTask::From(ktl::move(builder.value())));
}

fit::result<Errno, TaskBuilder> CurrentTask::create_init_child_process(
    const fbl::RefPtr<Kernel>& kernel, const ktl::string_view& initial_name) {
  LTRACE;
  util::WeakPtr<Task> weak_init = kernel->pids.Read()->get_task(1);
  fbl::RefPtr<Task> init_task = weak_init.Lock();
  if (!init_task) {
    return fit::error(errno(EINVAL));
  }

  auto task_info_factory = [kernel, initial_name](pid_t pid,
                                                  fbl::RefPtr<ProcessGroup> process_group) {
    return create_zircon_process(kernel, {}, pid, process_group, initial_name);
  };

  auto task =
      create_task(kernel, initial_name, init_task->fs()->fork(), task_info_factory) _EP(task);

  {
    auto init_writer = init_task->thread_group()->Write();
    auto new_process_writer = task->thread_group()->Write();
    new_process_writer->parent() = ThreadGroupParent::From(init_task->thread_group().get());
    init_writer->get_children().insert(util::WeakPtr(task->thread_group().get()));
  }

  // A child process created via fork(2) inherits its parent's
  // resource limits. Resource limits are preserved across execve(2).
  auto limits = init_task->thread_group()->limits.Lock();
  *task->thread_group()->limits.Lock() = *limits;

  return fit::ok(ktl::move(task.value()));
}

template <typename TaskInfoFactory>
fit::result<Errno, TaskBuilder> CurrentTask::create_task(const fbl::RefPtr<Kernel>& kernel,
                                                         const ktl::string_view& initial_name,
                                                         fbl::RefPtr<FsContext> root_fs,
                                                         TaskInfoFactory&& task_info_factory) {
  LTRACE;
  auto pids = kernel->pids.Write();
  auto pid = pids->allocate_pid();
  return create_task_with_pid(kernel, pids, pid, initial_name, root_fs, task_info_factory,
                              Credentials::root(),
                              fbl::Array<ktl::pair<starnix_uapi::Resource, uint64_t>>());
}

template <typename TaskInfoFactory>
fit::result<Errno, TaskBuilder> CurrentTask::create_task_with_pid(
    const fbl::RefPtr<Kernel>& kernel, RwLock<PidTable>::RwLockWriteGuard& pids, pid_t pid,
    const ktl::string_view& initial_name, fbl::RefPtr<FsContext> root_fs,
    TaskInfoFactory&& task_info_factory, Credentials creds,
    fbl::Array<ktl::pair<starnix_uapi::Resource, uint64_t>> rlimits) {
  LTRACE;
  DEBUG_ASSERT(pids->get_task(pid).Lock() == nullptr);

  fbl::RefPtr<ProcessGroup> process_group = ProcessGroup::New(pid, {});
  pids->add_process_group(process_group);

  auto task_info = task_info_factory(pid, process_group).value_or(TaskInfo{});

  process_group->insert(task_info.thread_group);

  // > The timer slack values of init (PID 1), the ancestor of all processes, are 50,000
  // > nanoseconds (50 microseconds).  The timer slack value is inherited by a child created
  // > via fork(2), and is preserved across execve(2).
  // https://man7.org/linux/man-pages/man2/prctl.2.html
  const uint64_t default_timerslack = 50000;

  auto builder = TaskBuilder{Task::New(
      pid, initial_name, task_info.thread_group, ktl::move(task_info.thread), FdTable::Create(),
      task_info.memory_manager, root_fs, creds, kSIGCHLD, SigSet(), false, default_timerslack)};

  // TODO (Herrera) Add fit::defer
  {
    auto temp_task = builder.task();
    _EP(builder->thread_group()->add(temp_task));

    for (auto& [resouce, limit] : rlimits) {
      builder->thread_group()->limits.Lock()->set(resouce,
                                                  rlimit{.rlim_cur = limit, .rlim_max = limit});
    }

    pids->add_task(temp_task);
    pids->add_thread_group(builder->thread_group());
  }

  return fit::ok(ktl::move(builder));
}

fit::result<Errno, TaskBuilder> CurrentTask::clone_task(uint64_t flags,
                                                        ktl::optional<Signal> child_exit_signal,
                                                        UserRef<pid_t> user_parent_tid,
                                                        UserRef<pid_t> user_child_tid) const {
  LTRACE;
  const uint64_t IMPLEMENTED_FLAGS =
      (CLONE_VM | CLONE_FS | CLONE_FILES | CLONE_SIGHAND | CLONE_THREAD | CLONE_SYSVSEM |
       CLONE_SETTLS | CLONE_PARENT_SETTID | CLONE_CHILD_CLEARTID | CLONE_CHILD_SETTID |
       CLONE_VFORK | CLONE_PTRACE);

  // A mask with all valid flags set, because we want to return a different error code for an
  // invalid flag vs an unimplemented flag. Subtracting 1 from the largest valid flag gives a
  // mask with all flags below it set. Shift up by one to make sure the largest flag is also
  // set.
  const uint64_t VALID_FLAGS = (CLONE_INTO_CGROUP << 1) - 1;

  auto clone_files = (flags & CLONE_FILES) != 0;
  auto clone_fs = (flags & CLONE_FS) != 0;
  auto clone_parent = (flags & CLONE_PARENT) != 0;
  auto clone_parent_settid = (flags & CLONE_PARENT_SETTID) != 0;
  auto clone_child_cleartid = (flags & CLONE_CHILD_CLEARTID) != 0;
  auto clone_child_settid = (flags & CLONE_CHILD_SETTID) != 0;
  auto clone_sysvsem = (flags & CLONE_SYSVSEM) != 0;
  auto clone_ptrace = (flags & CLONE_PTRACE) != 0;
  auto clone_thread = (flags & CLONE_THREAD) != 0;
  auto clone_vm = (flags & CLONE_VM) != 0;
  auto clone_sighand = (flags & CLONE_SIGHAND) != 0;
  auto clone_vfork = (flags & CLONE_VFORK) != 0;

  // auto new_uts = (flags & CLONE_NEWUTS) != 0;

  if (clone_ptrace) {
    // track_stub !(TODO("https://fxbug.dev/322874630"), "CLONE_PTRACE");
  }

  if (clone_sysvsem) {
    // track_stub !(TODO("https://fxbug.dev/322875185"), "CLONE_SYSVSEM");
  }

  if (clone_sighand && !clone_vm) {
    return fit::error(errno(EINVAL));
  }
  if (clone_thread && !clone_sighand) {
    return fit::error(errno(EINVAL));
  }
  if ((flags & ~VALID_FLAGS) != 0) {
    return fit::error(errno((EINVAL)));
  }

  if (clone_vm && !clone_thread) {
    // TODO(https://fxbug.dev/42066087) Implement CLONE_VM for child processes (not just child
    // threads). Currently this executes CLONE_VM (explicitly passed to clone() or as
    // used by vfork()) as a fork (the VM in the child is copy-on-write) which is almost
    // always OK.
    //
    // CLONE_VM is primarily as an optimization to avoid making a copy-on-write version of a
    // process' VM that will be immediately replaced with a call to exec(). The main users
    // (libc and language runtimes) don't actually rely on the memory being shared between
    // the two processes. And the vfork() man page explicitly allows vfork() to be
    // implemented as fork() which is what we do here.
    if (!clone_vfork) {
      // track_stub !(TODO("https://fxbug.dev/322875227"),"CLONE_VM without CLONE_THREAD or
      // CLONE_VFORK");
    }
  } else if (clone_thread && !clone_vm) {
    // track_stub !(TODO("https://fxbug.dev/322875167"), "CLONE_THREAD without CLONE_VM");
    return fit::error(errno(ENOSYS));
  }

  if ((flags & ~IMPLEMENTED_FLAGS) != 0) {
    // track_stub !(TODO("https://fxbug.dev/322875130"), "clone unknown flags", flags &
    // !IMPLEMENTED_FLAGS);
    return fit::error(errno(ENOSYS));
  }

  auto fs = clone_fs ? (*this)->fs() : (*this)->fs()->fork();
  auto files = clone_files ? (*this)->files() : (*this)->files().fork();

  auto kernel = (*this)->kernel();
  auto pids = kernel->pids.Write();

  pid_t pid;
  ktl::string_view command;
  Credentials creds;
  // let scheduler_policy;
  // let uts_ns;
  bool no_new_privs;
  // let seccomp_filters;
  // let robust_list_head = UserAddress::NULL.into();
  SigSet child_signal_mask;
  uint64_t timerslack_ns;
  // let uts_ns;
  // let security_state = security::task_alloc(&self, flags);

  auto task_info = [&]() -> fit::result<Errno, TaskInfo> {
    // These variables hold the original parent in case we need to switch the parent of the
    // new task because of CLONE_PARENT.
    ThreadGroupParent weak_original_parent;
    fbl::RefPtr<ThreadGroup> original_parent;

    // Make sure to drop these locks ASAP to avoid inversion
    // auto self = (*this);
    auto thread_group_state = [&]()
        -> fit::result<Errno, starnix_sync::RwLock<ThreadGroupMutableState>::RwLockWriteGuard> {
      auto tgs = task_->thread_group()->Write();
      if (clone_parent) {
        // With the CLONE_PARENT flag, the parent of the new task is our parent
        // instead of ourselves.
        if (!tgs->parent().has_value()) {
          return fit::error(errno(EINVAL));
        }
        weak_original_parent = *tgs->parent();
        original_parent = weak_original_parent.upgrade();
        return fit::ok(ktl::move(original_parent->Write()));
      }
      return fit::ok(ktl::move(tgs));
    }();

    if (thread_group_state.is_error()) {
      return thread_group_state.take_error();
    }

    auto state = task_->Read();

    no_new_privs = (*state).no_new_privs();

    // seccomp_filters = state.seccomp_filters.clone();

    child_signal_mask = (*state).signal_mask();

    pid = pids->allocate_pid();
    command = task_->command();
    creds = task_->creds();
    // scheduler_policy = state.scheduler_policy.fork();
    timerslack_ns = (*state).timerslack_ns_;

    /*
    uts_ns = if new_uts {
        if !self.creds().has_capability(CAP_SYS_ADMIN) {
            return error!(EPERM);
        }

        // Fork the UTS namespace of the existing task.
        let new_uts_ns = state.uts_ns.read().clone();
        Arc::new(RwLock::new(new_uts_ns))
    } else {
        // Inherit the UTS of the existing task.
        state.uts_ns.clone()
    };
    */

    if (clone_thread) {
      auto thread_group = task_->thread_group();
      auto memory_manager = task_->mm();
      return fit::ok(
          TaskInfo{.thread = {}, .thread_group = thread_group, .memory_manager = memory_manager});
    } else {
      // Drop the lock on this task before entering `create_zircon_process`, because it will
      // take a lock on the new thread group, and locks on thread groups have a higher
      // priority than locks on the task in the thread group.
      state.~RwLockGuard();
      /*
      let signal_actions = if clone_sighand {
          self.thread_group.signal_actions.clone()
      } else {
          self.thread_group.signal_actions.fork()
      };
      */
      auto process_group = thread_group_state->process_group();
      return create_zircon_process(kernel, ktl::move(*thread_group_state), pid, process_group,
                                   command);
    }
  }() _EP(task_info);

  // Only create the vfork event when the caller requested CLONE_VFORK.
  // let vfork_event = if clone_vfork { Some(Arc::new (zx::Event::create())) }
  // else {None};

  auto& [thread, thread_group, memory_manager] = task_info.value();

  auto child = TaskBuilder(Task::New(pid, command, ktl::move(thread_group), ktl::move(thread),
                                     files, ktl::move(memory_manager), fs, creds, child_exit_signal,
                                     child_signal_mask, no_new_privs, timerslack_ns));

  {
    auto child_task = child.task();
    // Drop the pids lock as soon as possible after creating the child. Destroying the child
    // and removing it from the pids table itself requires the pids lock, so if an early exit
    // takes place we have a self deadlock.
    pids->add_task(child_task);
    if (!clone_thread) {
      pids->add_thread_group(child->thread_group());
    }
    pids.~RwLockGuard();

    // Child lock must be taken before this lock. Drop the lock on the task, take a writable
    // lock on the child and take the current state back.

    /*
    #[cfg(any(test, debug_assertions))]
    */
    {
      // Take the lock on the thread group and its child in the correct order to ensure any wrong
      // ordering will trigger the tracing-mutex at the right call site.
      if (!clone_thread) {
        auto _l1 = task_->thread_group()->Read();
        auto _l2 = child->thread_group()->Read();
      }
    }

    if (clone_thread) {
      _EP(task_->thread_group()->add(child_task));
    } else {
      _EP(child->thread_group()->add(child_task));

      // These manipulations of the signal handling state appear to be related to
      // CLONE_SIGHAND and CLONE_VM rather than CLONE_THREAD. However, we do not support
      // all the combinations of these flags, which means doing these operations here
      // might actually be correct. However, if you find a test that fails because of the
      // placement of this logic here, we might need to move it.
      auto child_state = child->Write();
      auto state = task_->Read();
      // child_state.signals.alt_stack = state.signals.alt_stack;
      // child_state.signals.set_mask(state.signals.mask());
    }

    if (!clone_vm) {
      // We do not support running threads in the same process with different
      // MemoryManagers.
      ZX_ASSERT(!clone_thread);
      _EP(task_->mm()->snapshot_to(child->mm()));
    }

    if (clone_parent_settid) {
      _EP(this->write_object(user_parent_tid, child->id()));
    }

    if (clone_child_cleartid) {
      // child.write().clear_child_tid = user_child_tid;
    }

    if (clone_child_settid) {
      _EP(child->write_object(user_child_tid, child->id()));
    }

    // TODO(https://fxbug.dev/42066087): We do not support running different processes with
    // the same MemoryManager. Instead, we implement a rough approximation of that behavior
    // by making a copy-on-write clone of the memory from the original process.
    if (clone_vm && !clone_thread) {
      _EP(task_->mm()->snapshot_to(child->mm()));
    }

    child.thread_state() = this->thread_state().snapshot();
  }

  // Take the lock on thread group and task in the correct order to ensure any wrong ordering
  // will trigger the tracing-mutex at the right call site.
  // #[cfg(any(test, debug_assertions))]
  {
    auto _l1 = child->thread_group()->Read();
    auto _l2 = child->Read();
  }
  return fit::ok(ktl::move(child));
}

void CurrentTask::thread_group_exit(ExitStatus exit_status) {
  // self.ptrace_event(PtraceOptions::TRACEEXIT, exit_status.signal_info_status() as u64);
  task_->thread_group()->exit(exit_status, {});
}

starnix::testing::AutoReleasableTask CurrentTask::clone_task_for_test(
    uint64_t flags, ktl::optional<Signal> exit_signal) {
  auto result = clone_task(flags, exit_signal, mtl::DefaultConstruct<UserRef<pid_t>>(),
                           mtl::DefaultConstruct<UserRef<pid_t>>());
  ZX_ASSERT_MSG(result.is_ok(), "failed to create task in test");
  return starnix::testing::AutoReleasableTask::From(ktl::move(result.value()));
}

fit::result<Errno> CurrentTask::exec(const FileHandle& executable, const ktl::string_view& path,
                                     const fbl::Vector<BString>& argv,
                                     const fbl::Vector<BString>& environ) {
  LTRACEF_LEVEL(2, "path=[%.*s]\n", static_cast<int>(path.size()), path.data());

  // Executable must be a regular file
  /*
  if !executable.name.entry.node.is_reg() {
      return error!(EACCES);
  }
  */

  // File node must have EXEC mode permissions.
  // Note that the ability to execute a file is unrelated to the flags
  // used in the `open` call.
  /*
  executable.name.check_access(self, Access::EXEC)?;

  let elf_selinux_state = selinux_hooks::check_exec_access(self)?;
  */

  auto resolved_elf =
      resolve_executable(*this, executable, path, argv, environ /*,elf_selinux_state*/);
  if (resolved_elf.is_error()) {
    TRACEF("error in resolve_executable: %u\n", resolved_elf.error_value().error_code());
    return resolved_elf.take_error();
  }

  if (task_->thread_group()->Read()->tasks_count() > 1) {
    // track_stub !(TODO("https://fxbug.dev/297434895"), "exec on multithread process");
    return fit::error(errno(EINVAL));
  }

  auto err = finish_exec(path, resolved_elf.value());
  if (err.is_error()) {
    TRACEF("warning: unrecoverable error in exec: %u\n", err.error_value().error_code());
    /*
    send_standard_signal(
        self,
        SignalInfo { code: SI_KERNEL as i32, force: true, ..SignalInfo::default(SIGSEGV) },
    );
    */
    return err.take_error();
  }

  /*
    self.ptrace_event(PtraceOptions::TRACEEXEC, self.task.id as u64);
    self.signal_vfork();
  */

  return fit::ok();
}

fit::result<Errno> CurrentTask::finish_exec(const ktl::string_view& path,
                                            const ResolvedElf& resolved_elf) {
  LTRACEF_LEVEL(2, "path=[%.*s]\n", static_cast<int>(path.size()), path.data());

  //  Now that the exec will definitely finish (or crash), notify owners of
  //  locked futexes for the current process, which will be impossible to
  //  update after process image is replaced.  See get_robust_list(2).
  /*
    self.notify_robust_list();
  */

  auto exec_result = (*this)->mm()->exec(resolved_elf.file->name);
  if (exec_result.is_error()) {
    fit::error(errno(from_status_like_fdio(exec_result.error_value())));
  }

  // Update the SELinux state, if enabled.
  /*
  selinux_hooks::update_state_on_exec(self, &resolved_elf.selinux_state);
  */

  auto start_info = load_executable(*this, resolved_elf, path) _EP(start_info);
  auto regs = zx_thread_state_general_regs_t_from(start_info.value());
  thread_state_.registers = RegisterState::From(regs);

  {
    // Guard<Mutex> lock(task_->task_mutable_state_rw_lock());
    //  task_->persistent_info()->lock();

    // state.signals.alt_stack = None;
    // state.robust_list_head = UserAddress::NULL.into();

    // TODO(tbodt): Check whether capability xattrs are set on the file, and grant/limit
    // capabilities accordingly.
    // persistent_info.creds_mut().exec();
  }

  /*
    self.thread_state.extended_pstate.reset();

    self.thread_group.signal_actions.reset_for_exec();

    // TODO(http://b/320436714): when adding SELinux support for the file subsystem, implement
    // hook to clean up state after exec.

    // TODO: The termination signal is reset to SIGCHLD.

    // TODO(https://fxbug.dev/42082680): All threads other than the calling thread are
  destroyed.

    // TODO: The file descriptor table is unshared, undoing the effect of
    //       the CLONE_FILES flag of clone(2).
    //
    // To make this work, we can put the files in an RwLock and then cache
    // a reference to the files on the CurrentTask. That will let
    // functions that have CurrentTask access the FdTable without
    // needing to grab the read-lock.
    //
    // For now, we do not implement that behavior.
    self.files.exec();

    // TODO: POSIX timers are not preserved.
  */

  task_->thread_group()->Write()->did_exec() = true;

  // `prctl(PR_GET_NAME)` and `/proc/self/stat`
  /*
    let basename = if let Some(idx) = memchr::memrchr(b'/', path.to_bytes()) {
        // SAFETY: Substring of a CString will contain no null bytes.
        CString::new(&path.to_bytes()[idx + 1..]).unwrap()
    } else {
        path
    };
    set_zx_name(&fuchsia_runtime::thread_self(), basename.as_bytes());
    self.set_command_name(basename);
  */
  return fit::ok();
}

CurrentTask CurrentTask::From(TaskBuilder builder) {
  return CurrentTask::New(ktl::move(builder.task()), ktl::move(builder.thread_state()));
}

CurrentTask CurrentTask::New(fbl::RefPtr<Task> task, ThreadState thread_state) {
  return CurrentTask(task, thread_state);
}

util::WeakPtr<Task> CurrentTask::weak_task() const {
  ASSERT(task_);
  return util::WeakPtr<Task>(task_.get());
}

void CurrentTask::set_creds(Credentials creds) const {
  // Guard<Mutex> lock(persistent_info->lock());
  // persistent_info->state().creds = creds;
}

void CurrentTask::release() {
  LTRACE_ENTRY_OBJ;
  // if (task_ && task_->IsLastReference()) {
  if (task_) {
    // self.notify_robust_list();
    // let _ignored = self.clear_child_tid_if_needed();

    // We remove from the thread group here because the WeakRef in the pid
    // table to this task must be valid until this task is removed from the
    // thread group, but self.task.release() below invalidates it.
    task_->thread_group()->remove(task_);

    task_->release(thread_state_);
  }
  LTRACE_EXIT_OBJ;
}

fit::result<Errno, ktl::pair<NamespaceNode, FsStr>> CurrentTask::resolve_dir_fd(
    FdNumber dir_fd, FsStr path, ResolveFlags flags) const {
  LTRACEF_LEVEL(2, "dir_fd=%d, path=[%.*s]\n", dir_fd.raw(), static_cast<int>(path.length()),
                path.data());

  bool path_is_absolute = (path.size() > 1) && path[0] == '/';
  if (path_is_absolute) {
    if (flags.contains(ResolveFlagsEnum::BENEATH)) {
      return fit::error(errno(EXDEV));
    }
    path = ktl::string_view(path).substr(1);
  }

  auto dir_result = [this, &path_is_absolute, &flags,
                     &dir_fd]() -> fit::result<Errno, NamespaceNode> {
    if (path_is_absolute && !flags.contains(ResolveFlagsEnum::IN_ROOT)) {
      return fit::ok((*this)->fs()->root());
    } else if (dir_fd == FdNumber::AT_FDCWD_) {
      return fit::ok((*this)->fs()->cwd());
    } else {
      // O_PATH allowed for:
      //
      //   Passing the file descriptor as the dirfd argument of
      //   openat() and the other "*at()" system calls.  This
      //   includes linkat(2) with AT_EMPTY_PATH (or via procfs
      //   using AT_SYMLINK_FOLLOW) even if the file is not a
      //   directory.
      //
      // See https://man7.org/linux/man-pages/man2/open.2.html
      auto result = task_->files().get_allowing_opath(dir_fd);
      if (result.is_error()) {
        return result.take_error();
      }
      return fit::ok(result.value()->name);
    }
  }();
  if (dir_result.is_error())
    return dir_result.take_error();
  auto dir = dir_result.value();

  if (!path.empty()) {
    if (!dir.entry->node_->is_dir()) {
      return fit::error(errno(ENOTDIR));
    }
    if (auto check_access_result = dir.check_access(*this, Access(Access::EnumType::EXEC));
        check_access_result.is_error()) {
      return check_access_result.take_error();
    }
  }

  return fit::ok(ktl::pair(dir, path));
}

fit::result<Errno, FileHandle> CurrentTask::open_file(const FsStr& path, OpenFlags flags) const {
  LTRACEF_LEVEL(2, "path=[%.*s], flags=0x%x\n", static_cast<int>(path.length()), path.data(),
                flags.bits());
  if (flags.contains(OpenFlagsEnum::CREAT)) {
    // In order to support OpenFlags::CREAT we would need to take a
    // FileMode argument.
    return fit::error(errno(EINVAL));
  }
  return open_file_at(FdNumber::AT_FDCWD_, path, flags, FileMode(), ResolveFlags::empty());
}

fit::result<Errno, ktl::pair<NamespaceNode, bool>> CurrentTask::resolve_open_path(
    LookupContext& context, NamespaceNode dir, const FsStr& path, FileMode mode,
    OpenFlags flags) const {
  LTRACEF_LEVEL(2, "path=[%.*s], flags=0x%x\n", static_cast<int>(path.length()), path.data(),
                flags.bits());
  context.update_for_path(path);
  auto parent_content = context.with(SymlinkMode::Follow);
  auto lookup_parent_result = lookup_parent(parent_content, dir, path);
  if (lookup_parent_result.is_error())
    return lookup_parent_result.take_error();

  auto [parent, basename] = lookup_parent_result.value();

  context.remaining_follows = parent_content.remaining_follows;

  auto must_create = flags.contains(OpenFlagsEnum::CREAT) && flags.contains(OpenFlagsEnum::EXCL);

  // Lookup the child, without following a symlink or expecting it to be a directory.
  auto child_context = context.with(SymlinkMode::NoFollow);
  child_context.must_be_directory = false;

  if (auto lookup_child_result = parent.lookup_child(*this, child_context, basename);
      lookup_child_result.is_ok()) {
    auto name = lookup_child_result.value();
    if (name.entry->node_->is_lnk()) {
      if (flags.contains(OpenFlagsEnum::PATH) && context.symlink_mode == SymlinkMode::NoFollow) {
        // When O_PATH is specified in flags, if pathname is a symbolic link
        // and the O_NOFOLLOW flag is also specified, then the call returns
        // a file descriptor referring to the symbolic link.
        // See https://man7.org/linux/man-pages/man2/openat.2.html
        //
        // If the trailing component (i.e., basename) of
        // pathname is a symbolic link, how.resolve contains
        // RESOLVE_NO_SYMLINKS, and how.flags contains both
        // O_PATH and O_NOFOLLOW, then an O_PATH file
        // descriptor referencing the symbolic link will be
        // returned.
        // See https://man7.org/linux/man-pages/man2/openat2.2.html
        return fit::ok(ktl::pair(name, false));
      }

      if ((!flags.contains(OpenFlagsEnum::PATH) && context.symlink_mode == SymlinkMode::NoFollow) ||
          context.resolve_flags.contains(ResolveFlagsEnum::NO_SYMLINKS) ||
          context.remaining_follows == 0) {
        if (must_create) {
          // Since `must_create` is set, and a node was found, this returns EEXIST
          // instead of ELOOP.
          return fit::error(errno(EEXIST));
        }
        // A symlink was found, but one of the following is true:
        // * flags specified O_NOFOLLOW but not O_PATH.
        // * how.resolve contains RESOLVE_NO_SYMLINKS
        // * too many symlink traversals have been attempted
        return fit::error(errno(ELOOP));
      }

      context.remaining_follows -= 1;

      auto readlink_result = name.readlink(*this);
      if (readlink_result.is_error())
        return readlink_result.take_error();
      return ktl::visit(
          SymlinkTarget::overloaded{
              [&, p = ktl::move(parent)](
                  const FsString& path) -> fit::result<Errno, ktl::pair<NamespaceNode, bool>> {
                auto dir = (path[0] == '/') ? (*this)->fs()->root() : p;
                return resolve_open_path(context, dir, path, mode, flags);
              },
              [&](NamespaceNode node) -> fit::result<Errno, ktl::pair<NamespaceNode, bool>> {
                if (context.resolve_flags.contains(ResolveFlagsEnum::NO_MAGICLINKS)) {
                  return fit::error(errno(ELOOP));
                }
                return fit::ok(ktl::pair(node, false));
              },
          },
          readlink_result.value().value);

    } else {
      if (must_create) {
        return fit::error(errno(EEXIST));
      }
      return fit::ok(ktl::pair(name, false));
    }
  } else {
    auto _errno = lookup_child_result.error_value();
    if ((_errno == errno(ENOENT)) && flags.contains(OpenFlagsEnum::CREAT)) {
      if (context.must_be_directory) {
        return fit::error(errno(EISDIR));
      }
      auto open_create_node_result = parent.open_create_node(
          *this, basename, mode.with_type(FileMode::IFREG), DeviceType::NONE, flags);
      if (open_create_node_result.is_error())
        return open_create_node_result.take_error();

      return fit::ok(ktl::pair(open_create_node_result.value(), true));
    } else {
      return lookup_child_result.take_error();
    }
  }
}

fit::result<Errno, FileHandle> CurrentTask::open_file_at(FdNumber dir_fd, const FsStr& path,
                                                         OpenFlags flags, FileMode mode,
                                                         ResolveFlags resolve_flags) const {
  LTRACEF_LEVEL(2, "path=[%.*s], flags=0x%x, mode=0x%x, resolve_flags=0x%x\n",
                static_cast<int>(path.length()), path.data(), flags.bits(), mode.bits(),
                resolve_flags.bits());

  if (path.empty()) {
    return fit::error(errno(ENOENT));
  }

  auto result = resolve_dir_fd(dir_fd, path, resolve_flags);
  if (result.is_error()) {
    return result.take_error();
  }
  auto [dir, _path] = result.value();
  return open_namespace_node_at(dir, _path, flags, mode, resolve_flags);
}

fit::result<Errno, FileHandle> CurrentTask::open_namespace_node_at(
    NamespaceNode dir, const FsStr& path, OpenFlags _flags, FileMode mode,
    ResolveFlags& resolve_flags) const {
  LTRACEF_LEVEL(2, "path=[%.*s], flags=0x%x, mode=0x%x, resolve_flags=0x%x\n",
                static_cast<int>(path.length()), path.data(), _flags.bits(), mode.bits(),
                resolve_flags.bits());

  // 64-bit kernels force the O_LARGEFILE flag to be on.
  OpenFlagsImpl flags(_flags | OpenFlagsEnum::LARGEFILE);

  if (flags.contains(OpenFlagsEnum::PATH)) {
    // When O_PATH is specified in flags, flag bits other than O_CLOEXEC,
    // O_DIRECTORY, and O_NOFOLLOW are ignored.
    const OpenFlags ALLOWED_FLAGS = OpenFlags::from_bits_truncate(
        OpenFlags(OpenFlagsEnum::PATH).bits() | OpenFlags(OpenFlagsEnum::CLOEXEC).bits() |
        OpenFlags(OpenFlagsEnum::DIRECTORY).bits() | OpenFlags(OpenFlagsEnum::NOFOLLOW).bits());

    flags &= ALLOWED_FLAGS;
  }

  if (flags.contains(OpenFlagsEnum::TMPFILE) && !flags.can_write()) {
    return fit::error(errno(EINVAL));
  }

  bool nofollow = flags.contains(OpenFlagsEnum::NOFOLLOW);
  bool must_create = flags.contains(OpenFlagsEnum::CREAT) && flags.contains(OpenFlagsEnum::EXCL);

  auto symlink_mode = (nofollow || must_create) ? SymlinkMode::NoFollow : SymlinkMode::Follow;

  // Define the resolve_base variable
  ResolveBase resolve_base;
  bool beneath = resolve_flags.contains(ResolveFlagsEnum::BENEATH);
  bool in_root = resolve_flags.contains(ResolveFlagsEnum::IN_ROOT);

  if (!beneath && !in_root) /*(false, false)*/ {
    resolve_base = {.type = ResolveBaseType::None, .node = NamespaceNode()};
  } else if (beneath && !in_root) /*(true, false)*/ {
    resolve_base = {.type = ResolveBaseType::Beneath, .node = dir};
  } else if (!beneath && in_root) /* (false, true)*/ {
    resolve_base = {.type = ResolveBaseType::InRoot, .node = dir};
  } else {
    // Both flags are true, return error
    return fit::error(errno(EINVAL));
  }

  // `RESOLVE_BENEATH` and `RESOLVE_IN_ROOT` imply `RESOLVE_NO_MAGICLINKS`. This matches
  // Linux behavior. Strictly speaking it's is not really required, but it's hard to
  // implement `BENEATH` and `IN_ROOT` flags correctly otherwise.

  if (resolve_base.type != ResolveBaseType::None) {
    resolve_flags.insert(ResolveFlagsEnum::NO_MAGICLINKS);
  }

  auto context = LookupContext{
      .symlink_mode = symlink_mode,
      .remaining_follows = MAX_SYMLINK_FOLLOWS,
      .must_be_directory = flags.contains(OpenFlagsEnum::DIRECTORY),
      .resolve_flags = resolve_flags,
      .resolve_base = resolve_base,
  };

  auto result = resolve_open_path(context, dir, path, mode, flags);
  if (result.is_error()) {
    /*
      let mut abs_path = dir.path(&self.task);
      abs_path.extend(&**path);
      track_file_not_found(abs_path);
      return Err(e);
    */
    return result.take_error();
  }

  auto [name, created] = result.value();

  auto name_result = [&]() -> fit::result<Errno, NamespaceNode> {
    if (flags.contains(OpenFlagsEnum::TMPFILE)) {
      return name.create_tmpfile(*this, mode.with_type(FileMode::IFREG), flags);
    } else {
      auto mode_ = name.entry->node_->info()->mode;

      // These checks are not needed in the `O_TMPFILE` case because `mode` refers to the
      // file we are opening. With `O_TMPFILE`, that file is the regular file we just
      // created rather than the node we found by resolving the path.
      //
      // For example, we do not need to produce `ENOTDIR` when `must_be_directory` is set
      // because `must_be_directory` refers to the node we found by resolving the path.
      // If that node was not a directory, then `create_tmpfile` will produce an error.
      //
      // Similarly, we never need to call `truncate` because `O_TMPFILE` is newly created
      // and therefor already an empty file.

      if (nofollow && mode_.is_lnk()) {
        return fit::error(errno(ELOOP));
      }

      if (mode.is_dir()) {
      } else if (context.must_be_directory) {
        return fit::error(errno(ENOTDIR));
      }

      if (flags.contains(OpenFlagsEnum::TRUNC) && mode.is_reg() && !created) {
        // You might think we should check file.can_write() at this
        // point, which is what the docs suggest, but apparently we
        // are supposed to truncate the file if this task can write
        // to the underlying node, even if we are opening the file
        // as read-only. See OpenTest.CanTruncateReadOnly.
        if (auto truncate_result = name.truncate(*this, 0); truncate_result.is_error()) {
          return truncate_result.take_error();
        }
      }
      return fit::ok(name);
    }
  }();

  if (name_result.is_error())
    return name_result.take_error();

  // If the node has been created, the open operation should not verify access right:
  // From <https://man7.org/linux/man-pages/man2/open.2.html>
  //
  // > Note that mode applies only to future accesses of the newly created file; the
  // > open() call that creates a read-only file may well return a  read/write  file
  // > descriptor.

  auto _name = name_result.value();
  return _name.open(*this, flags, !created);
}

fit::result<Errno, ktl::pair<NamespaceNode, FsString>> CurrentTask::lookup_parent_at(
    LookupContext& context, FdNumber dir_fd, const FsStr& path) const {
  auto result = resolve_dir_fd(dir_fd, path, ResolveFlags::empty()) _EP(result);
  return lookup_parent(context, result->first, result->second);
}

fit::result<Errno, ktl::pair<NamespaceNode, FsString>> CurrentTask::lookup_parent(
    LookupContext& context, const NamespaceNode& dir, const FsStr& path) const {
  context.update_for_path(path);

  auto current_node = dir;
  auto split = SplitStringCopy(path, "/", kTrimWhitespace, kSplitWantNonEmpty);
  auto it = split.begin();
  FsString current_path_component = (it != split.end()) ? *it++ : "";
  for (; it != split.end(); ++it) {
    if (auto lookup_child_result =
            current_node.lookup_child(*this, context, current_path_component);
        lookup_child_result.is_error()) {
      return lookup_child_result.take_error();
    } else {
      current_node = lookup_child_result.value();
      current_path_component = *it;
    }
  }
  return fit::ok(ktl::pair(current_node, current_path_component));
}

fit::result<Errno, NamespaceNode> CurrentTask::lookup_path(LookupContext& context,
                                                           NamespaceNode dir,
                                                           const FsStr& path) const {
  LTRACEF_LEVEL(2, "path=[%.*s]\n", static_cast<int>(path.length()), path.data());

  auto lookup_parent_result = lookup_parent(context, dir, path);
  if (lookup_parent_result.is_error())
    return lookup_parent_result.take_error();
  auto [parent, basename] = lookup_parent_result.value();
  return parent.lookup_child(*this, context, basename);
}

fit::result<Errno, NamespaceNode> CurrentTask::lookup_path_from_root(const FsStr& path) const {
  LTRACEF_LEVEL(2, "path=[%.*s]\n", static_cast<int>(path.length()), path.data());

  LookupContext context = LookupContext::Default();
  return lookup_path(context, (*this)->fs()->root(), path);
}

fit::result<Errno, ktl::span<uint8_t>> CurrentTask::read_memory(UserAddress addr,
                                                                ktl::span<uint8_t>& bytes) const {
  return task_->mm()->unified_read_memory(*this, addr, bytes);
}

fit::result<Errno, ktl::span<uint8_t>> CurrentTask::read_memory_partial_until_null_byte(
    UserAddress addr, ktl::span<uint8_t>& bytes) const {
  return task_->mm()->unified_read_memory_partial_until_null_byte(*this, addr, bytes);
}

fit::result<Errno, ktl::span<uint8_t>> CurrentTask::read_memory_partial(
    UserAddress addr, ktl::span<uint8_t>& bytes) const {
  return task_->mm()->unified_read_memory_partial(*this, addr, bytes);
}

fit::result<Errno, size_t> CurrentTask::write_memory(UserAddress addr,
                                                     const ktl::span<const uint8_t>& bytes) const {
  return task_->mm()->unified_write_memory(*this, addr, bytes);
}

fit::result<Errno, size_t> CurrentTask::write_memory_partial(
    UserAddress addr, const ktl::span<const uint8_t>& bytes) const {
  return task_->mm()->unified_write_memory_partial(*this, addr, bytes);
}

fit::result<Errno, size_t> CurrentTask::zero(UserAddress addr, size_t length) const {
  return task_->mm()->unified_zero(*this, addr, length);
}

UserAddress CurrentTask::maximum_valid_address() const {
  return task_->mm()->maximum_valid_user_address;
}

}  // namespace starnix
