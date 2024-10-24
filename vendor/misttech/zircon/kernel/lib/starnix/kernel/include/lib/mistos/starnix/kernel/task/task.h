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
#include <lib/mistos/util/bitflags.h>
#include <lib/mistos/util/weak_wrapper.h>
#include <lib/starnix_sync/locks.h>

#include <utility>

#include <fbl/alloc_checker.h>
#include <fbl/ref_counted.h>
#include <fbl/ref_counted_upgradeable.h>
#include <fbl/ref_ptr.h>
#include <kernel/mutex.h>
#include <ktl/atomic.h>
#include <ktl/optional.h>
#include <ktl/string_view.h>
#include <ktl/unique_ptr.h>
#include <object/signal_observer.h>

class ThreadDispatcher;

namespace starnix {

class TaskBuilder;
class Kernel;
class FsContext;
struct ThreadState;

namespace testing {
TaskBuilder create_test_init_task(fbl::RefPtr<Kernel> kernel, fbl::RefPtr<FsContext> fs);
}

enum class TaskFlagsEnum : uint8_t {
  EXITED = 0x1,
  SIGNALS_AVAILABLE = 0x2,
  TEMPORARY_SIGNAL_MASK = 0x4,
  /// Whether the executor should dump the stack of this task when it exits.
  /// Currently used to implement ExitStatus::CoreDump.
  DUMP_ON_EXIT = 0x8,
};

using TaskFlags = Flags<TaskFlagsEnum>;

class AtomicTaskFlags {
 public:
  explicit AtomicTaskFlags(TaskFlags flags) : flags_(flags.bits()) {}

  TaskFlags load(ktl::memory_order order) const {
    uint8_t bits = flags_.load(order);
    // We only ever store values from a `TaskFlags`.
    return TaskFlags::from_bits_retain(bits);
  }

  TaskFlags swap(TaskFlags flags, ktl::memory_order ordering) {
    uint8_t old_bits = flags_.exchange(flags.bits(), ordering);
    // We only ever store values from a `TaskFlags`.
    return TaskFlags::from_bits_retain(old_bits);
  }

 private:
  ktl::atomic<uint8_t> flags_;
};

class TaskMutableState {
 private:
  // See https://man7.org/linux/man-pages/man2/set_tid_address.2.html
  UserRef<pid_t> clear_child_tid_;

  /// Signal handler related state. This is grouped together for when atomicity is needed during
  /// signal sending and delivery.
  SignalState signals_;

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
  bool no_new_privs_;

 public:
  /// Userspace hint about how to adjust the OOM score for this process.
  // int32_t oom_score_adj_;

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
  uint64_t timerslack_ns_;

  /// The default value for `timerslack_ns`. This value cannot change during the lifetime of a
  /// task.
  ///
  /// This value is set to the `timerslack_ns` of the creating thread, and thus is not constant
  /// across tasks.
  uint64_t default_timerslack_ns_;

  /// Information that a tracer needs to communicate with this process, if it
  /// is being traced.
  // ktl::optional<PtraceState> ptrace_;

  /// impl TaskMutableState
  bool no_new_privs() const { return no_new_privs_; }

  /// Sets the value of no_new_privs to true.  It is an error to set
  /// it to anything else.
  void enable_no_new_privs() { no_new_privs_ = true; }

  uint64_t get_timerslack_ns() { return timerslack_ns_; }

  /// Sets the current timerslack of the task to `ns`.
  ///
  /// If `ns` is zero, the current timerslack gets reset to the task's default timerslack.
  void set_timerslack_ns(uint64_t ns) {
    if (ns == 0) {
      timerslack_ns_ = default_timerslack_ns_;
    } else {
      timerslack_ns_ = ns;
    }
  }

  bool is_ptraced() { return false; }

  bool is_ptrace_listening() { return false; }

  /// Returns the task's currently active signal mask.
  SigSet signal_mask() const { return signals_.mask_; }

  /// Returns true if `signal` is currently blocked by this task's signal mask.
  bool is_signal_masked(Signal signal) const { return signals_.mask_.has_signal(signal); }

  /// Returns true if `signal` is blocked by the saved signal mask.
  ///
  /// Note that the current signal mask may still not be blocking the signal.
  bool is_signal_masked_by_saved_mask(Signal signal) const {
    auto saved_mask = signals_.saved_mask_;
    return saved_mask.has_value() && saved_mask->has_signal(signal);
  }

  /// Enqueues a signal at the back of the task's signal queue.
  // void enqueue_signal(const SignalInfo& signal) { signals_.enqueue(signal); }

  /// Enqueues the signal, allowing the signal to skip straight to the front of the task's queue.
  ///
  /// `enqueue_signal` is the more common API to use.
  ///
  /// Note that this will not guarantee that the signal is dequeued before any process-directed
  /// signals.
  // void enqueue_signal_front(const SignalInfo& signal) { signals_.enqueue_front(signal); }

  /// Sets the current signal mask of the task.
  void set_signal_mask(const SigSet& mask) { signals_.set_mask(mask); }

  /// Sets a temporary signal mask for the task.
  ///
  /// This mask should be removed by a matching call to `restore_signal_mask`.
  void set_temporary_signal_mask(const SigSet& mask) { signals_.set_temporary_mask(mask); }

  /// Removes the currently active, temporary, signal mask and restores the
  /// previously active signal mask.
  void restore_signal_mask() { signals_.restore_mask(); }

  /// Returns true if the task's current `RunState` is blocked.
  bool is_blocked() const { return signals_.run_state_.is_blocked(); }

  /// Sets the task's `RunState` to `run_state`.
  void set_run_state(RunState run_state) { signals_.run_state_ = run_state; }

  RunState run_state() const { return signals_.run_state_; }

  /*bool on_signal_stack(uint64_t stack_pointer_register) const {
    if (signals_.alt_stack().has_value()) {
      return sigaltstack_contains_pointer(signals_.alt_stack().value(), stack_pointer_register);
    }
    return false;
  }

  void set_sigaltstack(const ktl::optional<sigaltstack>& stack) { signals_.set_alt_stack(stack); }

  ktl::optional<sigaltstack> sigaltstack() const { return signals_.alt_stack(); }
*/

  // impl TaskMutableState<Base = Task>

  void update_flags(TaskFlags clear, TaskFlags set);

  void set_flags(TaskFlags flag, bool v) {
    TaskFlags clear = v ? TaskFlags::empty() : flag;
    TaskFlags set = v ? flag : TaskFlags::empty();

    update_flags(clear, set);
  }

  void set_exit_status(ExitStatus status) {
    set_flags(TaskFlags(TaskFlagsEnum::EXITED), true);
    exit_status_ = status;
  }

  void set_exit_status_if_not_already(ExitStatus status) {
    set_flags(TaskFlags(TaskFlagsEnum::EXITED), true);
    if (!exit_status_.has_value()) {
      exit_status_ = status;
    }
  }

  /// Returns whether or not a signal is pending for this task, taking the current
  /// signal mask into account.
  bool is_any_signal_pending() const;

 private:
  friend class Task;
  TaskMutableState(Task* base, UserRef<pid_t> clear_child_tid, SignalState signal,
                   ktl::optional<ExitStatus> exit_status, bool no_new_privs, uint64_t timerslack_ns,
                   uint64_t default_timerslack_ns)
      : clear_child_tid_(clear_child_tid),
        signals_(ktl::move(signal)),
        exit_status_(exit_status),
        no_new_privs_(no_new_privs),
        timerslack_ns_(timerslack_ns),
        default_timerslack_ns_(default_timerslack_ns),
        base_(base) {}

  Task* base_;
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

  // The memory manager for this task.
  ktl::optional<fbl::RefPtr<MemoryManager>> mm_;

  // The file system for this task.
  ktl::optional<starnix_sync::RwLock<fbl::RefPtr<FsContext>>> fs_;

  /// The namespace for abstract AF_UNIX sockets for this task.
  // pub abstract_socket_namespace: Arc<AbstractUnixSocketNamespace>,

  /// The namespace for AF_VSOCK for this task.
  // pub abstract_vsock_namespace: Arc<AbstractVsockSocketNamespace>,

  /// The stop state of the task, distinct from the stop state of the thread group.
  ///
  /// Must only be set when the `mutable_state` write lock is held.
  AtomicStopState stop_state_;

  /// The flags for the task.
  ///
  /// Must only be set the then `mutable_state` write lock is held.
  AtomicTaskFlags flags_;

  // The mutable state of the Task.
  mutable starnix_sync::RwLock<TaskMutableState> mutable_state_;

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

  /// impl Task
 public:
  fbl::RefPtr<Kernel>& kernel() const;

  bool has_same_address_space(const Task* other) const { return mm() == other->mm(); }

  TaskFlags flags() const { return flags_.load(std::memory_order_relaxed); }

  ktl::optional<ExitStatus> exit_status() const {
    if (!is_exited()) {
      return ktl::nullopt;
    }
    auto state = mutable_state_.Read();
    return state->exit_status_.has_value() ? state->exit_status_ : ktl::nullopt;
  }

  bool is_exited() const { return flags().contains(TaskFlagsEnum::EXITED); }

  StopState load_stopped() const { return stop_state_.load(std::memory_order_relaxed); }

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
                               Credentials creds, ktl::optional<Signal> exit_signal,
                               SigSet signal_mask, bool no_new_privs, uint64_t timerslack_ns);

  fit::result<Errno, FdNumber> add_file(FileHandle file, FdFlags flags) const;

  Credentials creds() const { return (persistent_info_->Lock())->creds(); }

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

  ktl::string_view command() const { return persistent_info_->Lock()->command(); }

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

  class ThreadSignalObserver final : public SignalObserver {
   public:
    ThreadSignalObserver(util::WeakPtr<Task> task) : SignalObserver(), task_(ktl::move(task)) {}
    ~ThreadSignalObserver() final = default;

   private:
    // |SignalObserver| implementation.
    void OnMatch(zx_signals_t signals) final;
    void OnCancel(zx_signals_t signals) final;

    fbl::Canary<fbl::magic("TTSO")> canary_;

    util::WeakPtr<Task> task_;
  };

  ThreadSignalObserver* observer() { return &observer_; }

  ~Task() override;

 private:
  friend class TaskMutableState;
  friend class CurrentTask;
  friend class ThreadGroup;
  friend class TaskContainer;

  friend TaskBuilder testing::create_test_init_task(fbl::RefPtr<Kernel> kernel,
                                                    fbl::RefPtr<FsContext> fs);

  DISALLOW_COPY_ASSIGN_AND_MOVE(Task);

  Task(pid_t id, fbl::RefPtr<ThreadGroup> thread_group,
       ktl::optional<fbl::RefPtr<ThreadDispatcher>> thread, FdTable files,
       ktl::optional<fbl::RefPtr<MemoryManager>> mm, ktl::optional<fbl::RefPtr<FsContext>> fs,
       ktl::optional<Signal> exit_signal, SigSet signal_mask, bool no_new_privs,
       uint64_t timerslack_ns);

  ThreadSignalObserver observer_;
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
        new (&ac) TaskContainer(util::WeakPtr<Task>(task.get()), task->persistent_info_));
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

  TaskPersistentInfo into() const { return info_; }

 private:
  TaskContainer(util::WeakPtr<Task> weak, TaskPersistentInfo& info)
      : weak_(ktl::move(weak)), info_(info) {}

  util::WeakPtr<Task> weak_;

  TaskPersistentInfo info_;
};

}  // namespace starnix

template <>
constexpr Flag<starnix::TaskFlagsEnum> Flags<starnix::TaskFlagsEnum>::FLAGS[] = {
    {starnix::TaskFlagsEnum::EXITED},
    {starnix::TaskFlagsEnum::SIGNALS_AVAILABLE},
    {starnix::TaskFlagsEnum::TEMPORARY_SIGNAL_MASK},
    {starnix::TaskFlagsEnum::DUMP_ON_EXIT},
};

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_TASK_H_
