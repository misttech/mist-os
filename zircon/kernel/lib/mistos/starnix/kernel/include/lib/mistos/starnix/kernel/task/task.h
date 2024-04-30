// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_TASK_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_TASK_H_

#include <lib/mistos/linux_uapi/typedefs.h>
#include <lib/mistos/starnix/kernel/task/forward.h>
#include <lib/mistos/starnix/kernel/vfs/fd_table.h>
#include <lib/mistos/starnix/kernel/vfs/module.h>
#include <lib/mistos/starnix_uapi/auth.h>
#include <lib/mistos/zx/thread.h>

#include <utility>

#include <fbl/canary.h>
#include <fbl/ref_counted.h>
#include <fbl/ref_ptr.h>
#include <fbl/string.h>
#include <kernel/mutex.h>
#include <ktl/optional.h>
#include <ktl/unique_ptr.h>

namespace starnix {

using namespace starnix_uapi;

enum class ExitStatusType { Exit, Kill, CoreDump, Stop, Continue };

class ExitStatus {
 public:
  ExitStatusType type;

#if 0
  // Define constructors for each variant
  explicit ExitStatus(uint8_t exit_value) : type(ExitStatusType::Exit), exit(exit_value) {}
  explicit ExitStatus(const SignalInfo& signal) : type(ExitStatusType::Kill), signal(signal) {}
  explicit ExitStatus(const SignalInfo& signal, PtraceEvent event)
      : type(ExitStatusType::CoreDump), signal(signal), event(event) {}
  explicit ExitStatus(const SignalInfo& signal, PtraceEvent event, bool isStop)
      : type(isStop ? ExitStatusType::Stop : ExitStatusType::Continue),
        signal(signal),
        event(event) {}

  // Define member variables for each variant
  union {
    uint8_t exit;
    SignalInfo signal;
  };
  PtraceEvent event;
#endif
};

class TaskMutableState {
 public:
  TaskMutableState(const TaskMutableState&) = delete;
  TaskMutableState& operator=(const TaskMutableState&) = delete;

  TaskMutableState() = default;

  bool Initialize();

 private:
  // See https://man7.org/linux/man-pages/man2/set_tid_address.2.html
  // pub clear_child_tid: UserRef<pid_t>,

  /// Signal handler related state. This is grouped together for when atomicity is needed during
  /// signal sending and delivery.
  // pub signals: SignalState,

  // The exit status that this task exited with.
  ktl::optional<ExitStatus> exit_status_;

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

  /// Bit that determines whether a newly started program can have privileges its parent does
  /// not have.  See Documentation/prctl/no_new_privs.txt in the Linux kernel for details.
  /// Note that Starnix does not currently implement the relevant privileges (e.g.,
  /// setuid/setgid binaries).  So, you can set this, but it does nothing other than get
  /// propagated to children.
  ///
  /// The documentation indicates that this can only ever be set to
  /// true, and it cannot be reverted to false.  Accessor methods
  /// for this field ensure this property.
  // no_new_privs: bool,

  /// Userspace hint about how to adjust the OOM score for this process.
  // pub oom_score_adj: i32,

  /// List of currently installed seccomp_filters
  // pub seccomp_filters: SeccompFilterContainer,

  /// A pointer to the head of the robust futex list of this thread in
  /// userspace. See get_robust_list(2)
  // pub robust_list_head: UserRef<robust_list_head>,

  /// The timer slack used to group timer expirations for the calling thread.
  ///
  /// Timers may expire up to `timerslack_ns` late, but never early.
  ///
  /// If this value is 0, the task's default timerslack is used.
  // pub timerslack_ns: u64,

  /// The default value for `timerslack_ns`. This value cannot change during the lifetime of a
  /// task.
  ///
  /// This value is set to the `timerslack_ns` of the creating thread, and thus is not constant
  /// across tasks.
  // pub default_timerslack_ns: u64,

  /// Information that a tracer needs to communicate with this process, if it
  /// is being traced.
  // pub ptrace: Option<PtraceState>,
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

struct TaskPersistentInfoState {
  /// Immutable information about the task
  pid_t tid_;
  pid_t pid_;

  /// The command of this task.
  fbl::String command_;

  /// The security credentials for this task.
  Credentials creds;

  /// The signal this task generates on exit.
  // exit_signal: Option<Signal>,
};

class TaskPersistentInfoLock : public fbl::RefCounted<TaskPersistentInfoLock> {
 public:
  TaskPersistentInfoLock(TaskPersistentInfoState state) : state_(std::move(state)) {}

  const TaskPersistentInfoState& state() const { return state_; }
  TaskPersistentInfoState& state() { return state_; }

  Lock<Mutex>* lock() const TA_RET_CAP(lock_) { return &lock_; }

 private:
  mutable DECLARE_MUTEX(TaskPersistentInfoLock) lock_;
  TaskPersistentInfoState state_ TA_GUARDED(lock_);
};

using TaskPersistentInfo = fbl::RefPtr<TaskPersistentInfoLock>;

class MemoryManager;

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

class Task : public fbl::RefCounted<Task> {
 public:
  const fbl::RefPtr<Kernel>& kernel() const;

  /// Internal function for creating a Task object. Useful when you need to specify the value of
  /// every field. create_process and create_thread are more likely to be what you want.
  ///
  /// Any fields that should be initialized fresh for every task, even if the task was created
  /// with fork, are initialized to their defaults inside this function. All other fields are
  /// passed as parameters.
  static zx_status_t New(pid_t pid, const fbl::String& command,
                         fbl::RefPtr<ThreadGroup> thread_group, ktl::optional<zx::thread> thread,
                         FdTable files, fbl::RefPtr<MemoryManager> mm, fbl::RefPtr<FsContext> fs,
                         fbl::RefPtr<Task>* out);

  const fbl::RefPtr<FsContext>& fs() const;
  const fbl::RefPtr<MemoryManager>& mm() const;

  Credentials creds() const {
    Guard<Mutex> lock(persistent_info_->lock());
    return persistent_info_->state().creds;
  }

  /*
    pub fn exit_signal(&self) -> Option<Signal> {
        self.persistent_info.lock().exit_signal
    }
  */

  void set_creds(Credentials creds) const {
    Guard<Mutex> lock(persistent_info_->lock());
    persistent_info_->state().creds = creds;
  }

  fbl::RefPtr<Task> get_task(pid_t pid);
  pid_t get_pid() const;
  pid_t get_tid() const;
  bool is_leader() const { return get_pid() == get_tid(); }

  // ucred as_ucred() const;
  FsCred as_fscred() const { return creds().as_fscred(); }

  // Acessors
  TaskPersistentInfo persistent_info() { return persistent_info_; }
  const fbl::RefPtr<ThreadGroup>& thread_group() const;
  FdTable& files() { return files_; }

  Lock<Mutex>* task_mutable_state_rw_lock() const TA_RET_CAP(task_mutable_state_rw_lock_) {
    return &task_mutable_state_rw_lock_;
  }

  Lock<Mutex>* thread_rw_lock() const TA_RET_CAP(thread_rw_lock_) { return &thread_rw_lock_; }

  // TaskMutableState& mutable_state() { return &mutable_state_; }

  ktl::optional<zx::thread>& thread() { return thread_; }

  ~Task();

 private:
  Task(pid_t id, fbl::RefPtr<ThreadGroup> thread_group, ktl::optional<zx::thread> thread,
       FdTable files, ktl::optional<fbl::RefPtr<MemoryManager>> mm,
       ktl::optional<fbl::RefPtr<FsContext>> fs, TaskPersistentInfoState persistent_info);

  fbl::Canary<fbl::magic("TASK")> canary_;

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
  mutable DECLARE_MUTEX(Task) thread_rw_lock_;
  ktl::optional<zx::thread> thread_ TA_GUARDED(thread_rw_lock_);

  // The file descriptor table for this task.
  //
  // This table can be share by many tasks.
  FdTable files_;

  // The memory manager for this task.
  ktl::optional<fbl::RefPtr<MemoryManager>> mm_;

  // The file system for this task.
  ktl::optional<fbl::RefPtr<FsContext>> fs_;

  /// The namespace for abstract AF_UNIX sockets for this task.
  // pub abstract_socket_namespace: Arc<AbstractUnixSocketNamespace>,

  /// The namespace for AF_VSOCK for this task.
  // pub abstract_vsock_namespace: Arc<AbstractVsockSocketNamespace>,

  /// The stop state of the task, distinct from the stop state of the thread group.
  ///
  /// Must only be set when the `mutable_state` write lock is held.
  // stop_state: AtomicStopState,

  /// The flags for the task.
  ///
  /// Must only be set the then `mutable_state` write lock is held.
  // flags: AtomicTaskFlags,

  // The mutable state of the Task.
  mutable DECLARE_MUTEX(Task) task_mutable_state_rw_lock_;
  TaskMutableState mutable_state_ TA_GUARDED(task_mutable_state_rw_lock_);

  // The information of the task that needs to be available to the `ThreadGroup` while computing
  // which process a wait can target.
  // Contains the command line, the task credentials and the exit signal.
  // See `TaskPersistentInfo` for more information.
  TaskPersistentInfo persistent_info_;

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
};

// NOTE: This class originaly was in thread_group.rs
/// Container around a weak task and a strong `TaskPersistentInfo`. It is needed to keep the
/// information even when the task is not upgradable, because when the task is dropped, there is a
/// moment where the task is not yet released, yet the weak pointer is not upgradeable anymore.
/// During this time, it is still necessary to access the persistent info to compute the state of
/// the thread for the different wait syscalls.
class TaskContainer : public fbl::RefCounted<TaskContainer>,
                      public fbl::WAVLTreeContainable<fbl::RefPtr<TaskContainer>> {
 public:
  TaskContainer(TaskPersistentInfo info) : info_(std::move(info)) {}

  // WAVL-tree Index
  uint GetKey() const { return info_->state().tid_; }

 private:
  // TODO WEAK_REF
  // fbl::RefPtr<Task> task;
  TaskPersistentInfo info_;
};

}  // namespace starnix

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_TASK_H_
