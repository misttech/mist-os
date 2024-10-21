// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_TASK_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_TASK_H_

#include <lib/fit/result.h>
#include <lib/mistos/linux_uapi/typedefs.h>
#include <lib/mistos/starnix/kernel/mm/memory_accessor.h>
#include <lib/mistos/starnix/kernel/signals/types.h>
#include <lib/mistos/starnix/kernel/task/exit_status.h>
#include <lib/mistos/starnix/kernel/vfs/fd_table.h>
#include <lib/mistos/starnix_uapi/auth.h>
#include <lib/mistos/util/weak_wrapper.h>
#include <lib/starnix_sync/locks.h>

#include <utility>

#include <fbl/alloc_checker.h>
#include <fbl/ref_counted.h>
#include <fbl/ref_counted_upgradeable.h>
#include <fbl/ref_ptr.h>
#include <kernel/mutex.h>
#include <ktl/optional.h>
#include <ktl/string_view.h>
#include <ktl/unique_ptr.h>

class ThreadDispatcher;

namespace starnix {

class TaskBuilder;
class Kernel;
class FsContext;
struct ThreadState;

namespace testing {
TaskBuilder create_test_init_task(fbl::RefPtr<Kernel> kernel, fbl::RefPtr<FsContext> fs);
}

class TaskMutableState {
 public:
  // See https://man7.org/linux/man-pages/man2/set_tid_address.2.html
  // UserRef<pid_t> clear_child_tid;

  /// Signal handler related state. This is grouped together for when atomicity is needed during
  /// signal sending and delivery.
  SignalState signals;

 private:
  // The exit status that this task exited with.
  ktl::optional<ExitStatus> exit_status_;

 public:
  /// Desired scheduler policy for the task.
  // pub scheduler_policy: SchedulerPolicy,

  /// The UTS namespace assigned to this thread.
  ///
  /// This field is kept in the mutable state because the UTS namespace of a thread
  /// can be forked using `clone()` or `unshare()` syscalls.
  ///
  /// We use UtsNamespaceHandle because the UTS properties can be modified
  /// by any other thread that shares this namespace.
  // pub uts_ns: UtsNamespaceHandle,

 private:
  /// Bit that determines whether a newly started program can have privileges its parent does
  /// not have.  See Documentation/prctl/no_new_privs.txt in the Linux kernel for details.
  /// Note that Starnix does not currently implement the relevant privileges (e.g.,
  /// setuid/setgid binaries).  So, you can set this, but it does nothing other than get
  /// propagated to children.
  ///
  /// The documentation indicates that this can only ever be set to
  /// true, and it cannot be reverted to false.  Accessor methods
  /// for this field ensure this property.
  bool no_new_privs_;

  /// Userspace hint about how to adjust the OOM score for this process.
  // int32_t oom_score_adj_;

  /// List of currently installed seccomp_filters
  // pub seccomp_filters: SeccompFilterContainer,

  /// A pointer to the head of the robust futex list of this thread in
  /// userspace. See get_robust_list(2)
  // pub robust_list_head: UserRef<robust_list_head>,

 public:
  /// The timer slack used to group timer expirations for the calling thread.
  ///
  /// Timers may expire up to `timerslack_ns` late, but never early.
  ///
  /// If this value is 0, the task's default timerslack is used.
  uint64_t timerslack_nsl;

  /// The default value for `timerslack_ns`. This value cannot change during the lifetime of a
  /// task.
  ///
  /// This value is set to the `timerslack_ns` of the creating thread, and thus is not constant
  /// across tasks.
  // pub default_timerslack_ns: u64,

  /// Information that a tracer needs to communicate with this process, if it
  /// is being traced.
  // pub ptrace: Option<PtraceState>,

 public:
  /// impl TaskMutableState
  bool no_new_privs() const { return no_new_privs_; }

 public:
  // TaskMutableState(const TaskMutableState&) = delete;
};

enum class TaskStateCode {
  // Task is being executed.
  Running,

  // Task is waiting for an event.
  Sleeping,

  // Tracing stop
  TracingStop,

  // Task has exited.
  Zombie
};

class TaskPersistentInfoState;
using TaskPersistentInfo = fbl::RefPtr<starnix_sync::StarnixMutex<TaskPersistentInfoState>>;

/// The information of the task that needs to be available to the `ThreadGroup` while computing
/// which process a wait can target. It is necessary to shared this data with the `ThreadGroup` so
/// that it is available while the task is being dropped and so is not accessible from a weak
/// pointer.
class TaskPersistentInfoState {
 private:
  /// Immutable information about the task
  pid_t tid_;

  pid_t pid_;

  /// The command of this task.
  ktl::string_view command_;

  /// The security credentials for this task.
  Credentials creds_;

  /// The signal this task generates on exit.
  ktl::optional<Signal> exit_signal_;

 public:
  /// impl TaskPersistentInfoState
  static TaskPersistentInfo New(pid_t tid, pid_t pid, const ktl::string_view& command,
                                const Credentials& creds, ktl::optional<Signal> exit_signal);

  pid_t tid() const { return tid_; }

  pid_t pid() const { return pid_; }

  ktl::string_view command() const { return command_; }

  Credentials creds() const { return creds_; }

  Credentials& creds_mut() { return creds_; }

  ktl::optional<Signal> exit_signal() const { return exit_signal_; }

 private:
  TaskPersistentInfoState(pid_t tid, pid_t pid, const ktl::string_view& command,
                          const Credentials& creds, ktl::optional<Signal> exit_signal)
      : tid_(tid),
        pid_(pid),
        command_(ktl::move(command)),
        creds_(ktl::move(creds)),
        exit_signal_(exit_signal) {}
};

class MemoryManager;
class Kernel;
class FsContext;
class ThreadGroup;

/// A unit of execution.
///
/// A task is the primary unit of execution in the Starnix kernel. Most tasks are *user* tasks,
/// which have an associated Zircon thread. The Zircon thread switches between restricted mode,
/// in which the thread runs userspace code, and normal mode, in which the thread runs Starnix
/// code.
///
/// Tasks track the resources used by userspace by referencing various objects, such as an
/// `FdTable`, a `MemoryManager`, and an `FsContext`. Many tasks can share references to these
/// objects. In principle, which objects are shared between which tasks can be largely arbitrary,
/// but there are common patterns of sharing. For example, tasks created with `pthread_create`
/// will share the `FdTable`, `MemoryManager`, and `FsContext` and are often called "threads" by
/// userspace programmers. Tasks created by `posix_spawn` do not share these objects and are often
/// called "processes" by userspace programmers. However, inside the kernel, there is no clear
/// definition of a "thread" or a "process".
///
/// During boot, the kernel creates the first task, often called `init`. The vast majority of other
/// tasks are created as transitive clones (e.g., using `clone(2)`) of that task. Sometimes, the
/// kernel will create new tasks from whole cloth, either with a corresponding userspace component
/// or to represent some background work inside the kernel.
///
/// See also `CurrentTask`, which represents the task corresponding to the thread that is currently
/// executing.

class Task : public fbl::RefCountedUpgradeable<Task>, public MemoryAccessorExt {
 private:
  // A unique identifier for this task.
  //
  // This value can be read in userspace using `gettid(2)`. In general, this value
  // is different from the value return by `getpid(2)`, which returns the `id` of the leader
  // of the `thread_group`.
  pid_t id_;

  // The thread group to which this task belongs.
  //
  // The group of tasks in a thread group roughly corresponds to the userspace notion of a
  // process.
  fbl::RefPtr<ThreadGroup> thread_group_;

  // A handle to the underlying Zircon thread object.
  //
  // Some tasks lack an underlying Zircon thread. These tasks are used internally by the
  // Starnix kernel to track background work, typically on a `kthread`.
  mutable starnix_sync::RwLock<ktl::optional<fbl::RefPtr<ThreadDispatcher>>> thread_;

  // The file descriptor table for this task.
  //
  // This table can be share by many tasks.
  FdTable files_;

 private:
  // The memory manager for this task.
  ktl::optional<fbl::RefPtr<MemoryManager>> mm_;

  // The file system for this task.
  ktl::optional<starnix_sync::RwLock<fbl::RefPtr<FsContext>>> fs_;

 public:
  /// The namespace for abstract AF_UNIX sockets for this task.
  // pub abstract_socket_namespace: Arc<AbstractUnixSocketNamespace>,

  /// The namespace for AF_VSOCK for this task.
  // pub abstract_vsock_namespace: Arc<AbstractVsockSocketNamespace>,

 private:
  /// The stop state of the task, distinct from the stop state of the thread group.
  ///
  /// Must only be set when the `mutable_state` write lock is held.
  // stop_state: AtomicStopState,

  /// The flags for the task.
  ///
  /// Must only be set the then `mutable_state` write lock is held.
  // flags: AtomicTaskFlags,

  // The mutable state of the Task.
  mutable starnix_sync::RwLock<TaskMutableState> mutable_state_;

 public:
  // The information of the task that needs to be available to the `ThreadGroup` while computing
  // which process a wait can target.
  // Contains the command line, the task credentials and the exit signal.
  // See `TaskPersistentInfo` for more information.
  TaskPersistentInfo persistent_info;

  /// For vfork and clone() with CLONE_VFORK, this is set when the task exits or calls execve().
  /// It allows the calling task to block until the fork has been completed. Only populated
  /// when created with the CLONE_VFORK flag.
  // vfork_event: Option<Arc<zx::Event>>,

  /// Variable that can tell you whether there are currently seccomp
  /// filters without holding a lock
  // pub seccomp_filter_state: SeccompState,

  /// Used to ensure that all logs related to this task carry the same metadata about the task.
  // logging_span: OnceCell<starnix_logging::Span>,

  /// Tell you whether you are tracing syscall entry / exit without a lock.
  // pub trace_syscalls: AtomicBool,

  /// impl Task
  fbl::RefPtr<Kernel>& kernel() const;

  /// Upgrade a Reference to a Task, returning a ESRCH errno if the reference cannot be borrowed.
  static fit::result<Errno, fbl::RefPtr<Task>> from_weak(util::WeakPtr<Task> weak) {
    fbl::RefPtr<Task> task = weak.Lock();
    if (!task) {
      return fit::error(errno(ESRCH));
    }
    return fit::ok(task);
  }

  /// Internal function for creating a Task object. Useful when you need to specify the value of
  /// every field. create_process and create_thread are more likely to be what you want.
  ///
  /// Any fields that should be initialized fresh for every task, even if the task was created
  /// with fork, are initialized to their defaults inside this function. All other fields are
  /// passed as parameters.
  static fbl::RefPtr<Task> New(pid_t pid, const ktl::string_view& command,
                               fbl::RefPtr<ThreadGroup> thread_group,
                               ktl::optional<fbl::RefPtr<ThreadDispatcher>> thread, FdTable files,
                               fbl::RefPtr<MemoryManager> mm, fbl::RefPtr<FsContext> fs,
                               Credentials creds, ktl::optional<Signal> exit_signal);

  fit::result<Errno, FdNumber> add_file(FileHandle file, FdFlags flags) const;

  Credentials creds() const { return (persistent_info->Lock())->creds(); }

  /*
    pub fn exit_signal(&self) -> Option<Signal> {
        self.persistent_info.lock().exit_signal
    }
  */

  fbl::RefPtr<FsContext> fs() const;

  const fbl::RefPtr<MemoryManager>& mm() const;

  util::WeakPtr<Task> get_task(pid_t pid) const;

  pid_t get_pid() const;

  pid_t get_tid() const { return id(); }

  bool is_leader() const { return get_pid() == get_tid(); }

  // ucred as_ucred() const;

  FsCred as_fscred() const { return creds().as_fscred(); }

  ktl::string_view command() const { return persistent_info->Lock()->command(); }

  /// impl Releasable for Task
  void release(ThreadState context);

  /// impl MemoryAccessor for Task
  fit::result<Errno, ktl::span<uint8_t>> read_memory(UserAddress addr,
                                                     ktl::span<uint8_t>& bytes) const final;

  fit::result<Errno, ktl::span<uint8_t>> read_memory_partial_until_null_byte(
      UserAddress addr, ktl::span<uint8_t>& bytes) const final;

  fit::result<Errno, ktl::span<uint8_t>> read_memory_partial(UserAddress addr,
                                                             ktl::span<uint8_t>& bytes) const final;

  fit::result<Errno, size_t> write_memory(UserAddress addr,
                                          const ktl::span<const uint8_t>& bytes) const final;

  fit::result<Errno, size_t> write_memory_partial(
      UserAddress addr, const ktl::span<const uint8_t>& bytes) const final;

  fit::result<Errno, size_t> zero(UserAddress addr, size_t length) const final;

  // C++

  starnix_sync::RwLock<TaskMutableState>::RwLockReadGuard Read() const {
    return mutable_state_.Read();
  }

  starnix_sync::RwLock<TaskMutableState>::RwLockWriteGuard Write() const {
    return mutable_state_.Write();
  }

  pid_t id() const { return id_; }

  const fbl::RefPtr<ThreadGroup>& thread_group() const { return thread_group_; }

  const starnix_sync::RwLock<ktl::optional<fbl::RefPtr<ThreadDispatcher>>>& thread() const {
    return thread_;
  }
  starnix_sync::RwLock<ktl::optional<fbl::RefPtr<ThreadDispatcher>>>& thread() { return thread_; }

  const FdTable& files() const { return files_; }
  FdTable& files() { return files_; }

  ~Task() override;

 private:
  friend class CurrentTask;
  friend class ThreadGroup;
  friend TaskBuilder testing::create_test_init_task(fbl::RefPtr<Kernel> kernel,
                                                    fbl::RefPtr<FsContext> fs);

  DISALLOW_COPY_ASSIGN_AND_MOVE(Task);

  Task(pid_t id, fbl::RefPtr<ThreadGroup> thread_group,
       ktl::optional<fbl::RefPtr<ThreadDispatcher>> thread, FdTable files,
       ktl::optional<fbl::RefPtr<MemoryManager>> mm, ktl::optional<fbl::RefPtr<FsContext>> fs);
};

// NOTE: This class originaly was in thread_group.rs
/// Container around a weak task and a strong `TaskPersistentInfo`. It is needed to keep the
/// information even when the task is not upgradable, because when the task is dropped, there is a
/// moment where the task is not yet released, yet the weak pointer is not upgradeable anymore.
/// During this time, it is still necessary to access the persistent info to compute the state of
/// the thread for the different wait syscalls.
class TaskContainer : public fbl::WAVLTreeContainable<ktl::unique_ptr<TaskContainer>> {
 public:
  static ktl::unique_ptr<TaskContainer> From(fbl::RefPtr<Task> task) {
    fbl::AllocChecker ac;
    ktl::unique_ptr<TaskContainer> ptr = ktl::unique_ptr<TaskContainer>(
        new (&ac) TaskContainer(util::WeakPtr<Task>(task.get()), task->persistent_info));
    ZX_ASSERT(ac.check());
    return ptr;
  }

  // WAVL-tree Index
  pid_t GetKey() const { return (info_->Lock())->tid(); }

  // impl TaskContainer
  ktl::optional<fbl::RefPtr<Task>> upgrade() const {
    auto strong = weak_.Lock();
    if (strong) {
      return {ktl::move(strong)};
    }
    return ktl::nullopt;
  }

  util::WeakPtr<Task> weak_clone() const { return weak_; }

  starnix_sync::MutexGuard<TaskPersistentInfoState> info() const { return info_->Lock(); }

  operator TaskPersistentInfoState() const { return *info_->Lock(); }

 private:
  TaskContainer(util::WeakPtr<Task> weak, TaskPersistentInfo& info)
      : weak_(ktl::move(weak)), info_(info) {}

  util::WeakPtr<Task> weak_;

  TaskPersistentInfo info_;
};

}  // namespace starnix

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_TASK_H_
