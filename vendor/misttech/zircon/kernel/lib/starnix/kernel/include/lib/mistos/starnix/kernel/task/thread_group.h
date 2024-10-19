// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_THREAD_GROUP_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_THREAD_GROUP_H_

#include <lib/fit/result.h>
#include <lib/mistos/linux_uapi/typedefs.h>
#include <lib/mistos/starnix/kernel/mm/flags.h>
#include <lib/mistos/starnix/kernel/task/current_task.h>
#include <lib/mistos/starnix/kernel/task/exit_status.h>
#include <lib/mistos/starnix/kernel/task/zombie_process.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/mistos/starnix_uapi/resource_limits.h>
#include <lib/mistos/starnix_uapi/stats.h>
#include <lib/mistos/util/weak_wrapper.h>
#include <lib/starnix_sync/locks.h>
#include <zircon/assert.h>

#include <fbl/ref_counted.h>
#include <fbl/ref_ptr.h>
#include <fbl/vector.h>
#include <ktl/optional.h>
#include <object/handle.h>

class ProcessDispatcher;

namespace starnix {

class TaskContainer;
class Kernel;
class ProcessGroup;
class ThreadGroup;
class Task;
class WaitingOptions;

namespace internal {
// ProcessGroupMutableState
struct ProcessGroupTag {};
// ThreadGroupMutableState
struct ThreadGroupTag {};
}  // namespace internal

// Represents the exit information of a process
struct ProcessExitInfo {
  ExitStatus status;
  ktl::optional<Signal> exit_signal;
};

// Represents the result of a wait operation
struct WaitResult {
  pid_t pid;
  uid_t uid;
  ProcessExitInfo exit_info;
  TaskTimeStats time_stats;

  // Converts the wait result to a signal info
  /*SignalInfo AsSignalInfo() const {
    return SignalInfo::new_sigchld(pid, uid, exit_info.status.SignalInfoStatus(),
                                   exit_info.status.SignalInfoCode());
  }*/
};

// Represents the result of checking for a waitable child
class WaitableChildResult {
 public:
  enum class Type { ReadyNow, ShouldWait, NoneFound };

  static WaitableChildResult ReadyNow(WaitResult result) {
    return WaitableChildResult(Type::ReadyNow, ktl::move(result));
  }

  static WaitableChildResult ShouldWait() { return WaitableChildResult(Type::ShouldWait); }

  static WaitableChildResult NoneFound() { return WaitableChildResult(Type::NoneFound); }

  Type GetType() const { return type_; }

  WaitResult GetResult() const {
    ZX_ASSERT(type_ == Type::ReadyNow);
    return result_.value();
  }

 private:
  WaitableChildResult(Type type) : type_(type) {}
  WaitableChildResult(Type type, WaitResult result) : type_(type), result_(ktl::move(result)) {}

  Type type_;
  ktl::optional<WaitResult> result_;
};

class ThreadGroupParent {
 public:
  static ThreadGroupParent New(util::WeakPtr<ThreadGroup> t) {
    DEBUG_ASSERT(t.Lock());
    return ThreadGroupParent(t);
  }

  fbl::RefPtr<ThreadGroup> upgrade() const {
    auto ret = inner_.Lock();
    ZX_ASSERT_MSG(ret, "ThreadGroupParent references must always be valid");
    return ret;
  }

#if 0
  ThreadGroupParent& operator=(ThreadGroupParent&& other) noexcept {
    inner_ = std::move(other.inner_);
    return *this;
  }
#endif

  template <typename I>
  static ThreadGroupParent From(I&& r) {
    return ThreadGroupParent(util::WeakPtr<ThreadGroup>(std::forward<I>(r)));
  }

 private:
  ThreadGroupParent(util::WeakPtr<ThreadGroup> t) : inner_(std::move(t)) {}

  util::WeakPtr<ThreadGroup> inner_;
};

/// A selector that can match a process. Works as a representation of the pid argument to syscalls
/// like wait and kill.
class ProcessSelector {
 public:
  /// Matches any process at all.
  struct Any {};

  /// Matches only the process with the specified pid
  struct Pid {
    pid_t value;
  };

  /// Matches all the processes in the given process group
  struct Pgid {
    pid_t value;
  };

  using Variant = ktl::variant<Any, Pid, Pgid>;

  const Variant& selector() const { return selector_; }

  static ProcessSelector AnyProcess() { return ProcessSelector(Any{}); }
  static ProcessSelector SpecificPid(pid_t pid) { return ProcessSelector(Pid{pid}); }
  static ProcessSelector ProcessGroup(pid_t pgid) { return ProcessSelector(Pgid{pgid}); }

  bool DoMatch(pid_t pid, const PidTable& pid_table) const;

  // Helpers from the reference documentation for ktl::visit<>, to allow
  // visit-by-overload of the ktl::variant<> returned by GetLastReference():
  template <class... Ts>
  struct overloaded : Ts... {
    using Ts::operator()...;
  };
  // explicit deduction guide (not needed as of C++20)
  template <class... Ts>
  overloaded(Ts...) -> overloaded<Ts...>;

 private:
  explicit ProcessSelector(Variant selector) : selector_(ktl::move(selector)) {}

  Variant selector_;
};

/// The mutable state of the ThreadGroup.
class ThreadGroupMutableState {
 public:
  using BTreeMapTaskContainer = fbl::WAVLTree<pid_t, ktl::unique_ptr<TaskContainer>>;
  using BTreeMapThreadGroup =
      fbl::TaggedWAVLTree<pid_t, util::WeakPtr<ThreadGroup>, internal::ThreadGroupTag>;

 private:
  // The parent thread group.
  //
  // The value needs to be writable so that it can be re-parent to the correct subreaper if the
  // parent ends before the child.
  ktl::optional<ThreadGroupParent> parent_;

  // The tasks in the thread group.
  //
  // The references to Task is weak to prevent cycles as Task have a Arc reference to their
  // thread group.
  // It is still expected that these weak references are always valid, as tasks must unregister
  // themselves before they are deleted.
  BTreeMapTaskContainer tasks_;

  // The children of this thread group.
  //
  // The references to ThreadGroup is weak to prevent cycles as ThreadGroup have a Arc reference
  // to their parent.
  // It is still expected that these weak references are always valid, as thread groups must
  // unregister themselves before they are deleted.
  BTreeMapThreadGroup children_;

  /// Child tasks that have exited, but not yet been waited for.
  fbl::Vector<fbl::RefPtr<ZombieProcess>> zombie_children_;

  /// ptracees of this process that have exited, but not yet been waited for.
  // ZombiePtraces zombie_ptracees;

  // Child tasks that have exited, but the zombie ptrace needs to be consumed
  // before they can be waited for.  (pid_t, pid_t) is the original tracer and
  // tracee, so the tracer can be updated with a reaper if this thread group
  // exits.
  // fbl::Vector<std::pair<pid_t, pid_t>> deferred_zombie_ptracers;

  /// WaitQueue for updates to the WaitResults of tasks in this group.
  // WaitQueue child_status_waiters;

  /// Whether this thread group will inherit from children of dying processes in its descendant
  /// tree.
  // bool is_child_subreaper = false;

  /// The IDs used to perform shell job control.
  fbl::RefPtr<ProcessGroup> process_group_;

  /// The timers for this thread group (from timer_create(), etc.).
  // pub timers: TimerTable,

  bool did_exec_ = false;

  /// Wait queue for updates to `stopped`.
  // pub stopped_waiters: WaitQueue,

  /// A signal that indicates whether the process is going to become waitable
  /// via waitid and waitpid for either WSTOPPED or WCONTINUED, depending on
  /// the value of `stopped`. If not None, contains the SignalInfo to return.
  // Last signal received by the thread group
  ktl::optional<SignalInfo> last_signal_;

  // Exit information for the thread group leader
  ktl::optional<ProcessExitInfo> leader_exit_info_;

  bool terminating_ = false;

  /// The SELinux operations for this thread group.
  // pub selinux_state: Option<SeLinuxThreadGroupState>,

  /// Time statistics accumulated from the children.
  // pub children_time_stats: TaskTimeStats,

  /// Personality flags set with `sys_personality()`.
  // pub personality: PersonalityFlags,

  /// Thread groups allowed to trace tasks in this this thread group.
  // pub allowed_ptracers: PtraceAllowedPtracers,

 public:
  /// impl ThreadGroupMutableState<Base = ThreadGroup>
  pid_t leader() const;

  fbl::Vector<fbl::RefPtr<ThreadGroup>> children() const;

  fbl::Vector<fbl::RefPtr<Task>> tasks() const;

  fbl::Vector<pid_t> task_ids() const;

  bool contains_task(pid_t tid) const;

  fbl::RefPtr<Task> get_task(pid_t tid) const;

  size_t tasks_count() const { return tasks_.size(); }

  pid_t get_ppid() const;

  void set_process_group(fbl::RefPtr<ProcessGroup> new_process_group, PidTable* pids);

  // Removes this thread group from its current process group
  void leave_process_group(PidTable& pids);

  // Indicates whether the thread group is waitable via waitid and waitpid for
  /// either WSTOPPED or WCONTINUED.
  bool is_waitable() const;

  // Returns true if the exit signal matches the wait options for clone or non-clone processes
  static bool is_correct_exit_signal(bool wait_for_clone, ktl::optional<Signal> exit_signal);

  WaitableChildResult get_waitable_running_children(ProcessSelector selector,
                                                    const WaitingOptions& options,
                                                    const PidTable& pids) const;

  /// Returns any waitable child matching the given `selector` and `options`. Returns None if no
  /// child matching the selector is waitable. Returns ECHILD if no child matches the selector at
  /// all.
  ///
  /// Will remove the waitable status from the child depending on `options`.
  WaitableChildResult get_waitable_child(ProcessSelector selector, const WaitingOptions& options,
                                         PidTable& pids);

  // C++
  const fbl::Vector<fbl::RefPtr<ZombieProcess>>& get_zombie_children() const {
    return zombie_children_;
  }

  const ktl::optional<ThreadGroupParent>& parent() const { return parent_; }
  ktl::optional<ThreadGroupParent>& parent() { return parent_; }

  const fbl::RefPtr<ProcessGroup>& process_group() const { return process_group_; }

  const bool& did_exec() const { return did_exec_; }
  bool& did_exec() { return did_exec_; }

  const bool& terminating() const { return terminating_; }
  bool& terminating() { return terminating_; }

  ThreadGroupMutableState();
  ThreadGroupMutableState(ThreadGroup* base, ktl::optional<ThreadGroupParent> parent,
                          fbl::RefPtr<ProcessGroup> process_group);

 private:
  friend class ThreadGroup;

  ThreadGroup* base_ = nullptr;
};

/// A collection of `Task` objects that roughly correspond to a "process".
///
/// Userspace programmers often think about "threads" and "process", but those concepts have no
/// clear analogs inside the kernel because tasks are typically created using `clone(2)`, which
/// takes a complex set of flags that describes how much state is shared between the original task
/// and the new task.
///
/// If a new task is created with the `CLONE_THREAD` flag, the new task will be placed in the same
/// `ThreadGroup` as the original task. Userspace typically uses this flag in conjunction with the
/// `CLONE_FILES`, `CLONE_VM`, and `CLONE_FS`, which corresponds to the userspace notion of a
/// "thread". For example, that's how `pthread_create` behaves. In that sense, a `ThreadGroup`
/// normally corresponds to the set of "threads" in a "process". However, this pattern is purely a
/// userspace convention, and nothing stops userspace from using `CLONE_THREAD` without
/// `CLONE_FILES`, for example.
///
/// In Starnix, a `ThreadGroup` corresponds to a Zicon process, which means we do not support the
/// `CLONE_THREAD` flag without the `CLONE_VM` flag. If we run into problems with this limitation,
/// we might need to revise this correspondence.
///
/// Each `Task` in a `ThreadGroup` has the same thread group ID (`tgid`). The task with the same
/// `pid` as the `tgid` is called the thread group leader.
///
/// Thread groups are destroyed when the last task in the group exits.
class ThreadGroup
    : public fbl::RefCountedUpgradeable<ThreadGroup>,
      public fbl::ContainableBaseClasses<
          fbl::TaggedWAVLTreeContainable<util::WeakPtr<ThreadGroup>, internal::ProcessGroupTag>,
          fbl::TaggedWAVLTreeContainable<util::WeakPtr<ThreadGroup>, internal::ThreadGroupTag>> {
 private:
  /// Weak reference to the `OwnedRef` of this `ThreadGroup`. This allows to retrieve the
  /// `TempRef` from a raw `ThreadGroup`.
  util::WeakPtr<ThreadGroup> weak_thread_group_;

  // The kernel to which this thread group belongs.
  fbl::RefPtr<Kernel> kernel_;

  /// A handle to the underlying Zircon process object.
  ///
  /// Currently, we have a 1-to-1 mapping between thread groups and zx::process
  /// objects. This approach might break down if/when we implement CLONE_VM
  /// without CLONE_THREAD because that creates a situation where two thread
  /// groups share an address space. To implement that situation, we might
  /// need to break the 1-to-1 mapping between thread groups and zx::process
  /// or teach zx::process to share address spaces.
  KernelHandle<ProcessDispatcher> process_;

  /// The lead task of this thread group.
  ///
  /// The lead task is typically the initial thread created in the thread group.
  pid_t leader_;

  /// The signal actions that are registered for this process.
  // pub signal_actions: Arc<SignalActions>,

  /// A mechanism to be notified when this `ThreadGroup` is destroyed.
  // pub drop_notifier: DropNotifier,

  /// Whether the process is currently stopped.
  ///
  /// Must only be set when the `mutable_state` write lock is held.
  AtomicStopState stop_state_;

 private:
  /// The mutable state of the ThreadGroup.
  mutable starnix_sync::RwLock<ThreadGroupMutableState> mutable_state_;

 public:
  /// The resource limits for this thread group.  This is outside mutable_state
  /// to avoid deadlocks where the thread_group lock is held when acquiring
  /// the task lock, and vice versa.
  // pub limits: Mutex<ResourceLimits>,
  mutable starnix_sync::StarnixMutex<starnix_uapi::ResourceLimits> limits;

  /// The next unique identifier for a seccomp filter.  These are required to be
  /// able to distinguish identical seccomp filters, which are treated differently
  /// for the purposes of SECCOMP_FILTER_FLAG_TSYNC.  Inherited across clone because
  /// seccomp filters are also inherited across clone.
  // pub next_seccomp_filter_id: AtomicU64Counter,

 private:
  /// Timer id of ITIMER_REAL.
  // itimer_real_id: TimerId,

 public:
  /// Tasks ptraced by this process
  // pub ptracees: Mutex<BTreeMap<pid_t, TaskContainer>>,

  /// impl ThreadGroup
  static fbl::RefPtr<ThreadGroup> New(
      fbl::RefPtr<Kernel> kernel, KernelHandle<ProcessDispatcher> process,
      ktl::optional<starnix_sync::RwLock<ThreadGroupMutableState>::RwLockWriteGuard> parent,
      pid_t leader, fbl::RefPtr<ProcessGroup> process_group);

  StopState load_stopped() const { return stop_state_.load(std::memory_order_relaxed); }

  // Causes the thread group to exit.  If this is being called from a task
  // that is part of the current thread group, the caller should pass
  // `current_task`.  If ownership issues prevent passing `current_task`, then
  // callers should use CurrentTask::thread_group_exit instead.
  void exit(ExitStatus exit_status, ktl::optional<CurrentTask> current_task);

  uint64_t get_rlimit(starnix_uapi::Resource resource) const;

  fit::result<Errno> add(fbl::RefPtr<Task> task) const;

  void remove(fbl::RefPtr<Task> task) const;

  // Sets the session ID for this thread group
  fit::result<Errno> setsid() const;

  /// state_accessor!(ThreadGroup, mutable_state, Arc<ThreadGroup>);
  starnix_sync::RwLock<ThreadGroupMutableState>::RwLockReadGuard Read() const {
    return mutable_state_.Read();
  }

  starnix_sync::RwLock<ThreadGroupMutableState>::RwLockWriteGuard Write() const {
    return mutable_state_.Write();
  }

  /// impl Releasable for ThreadGroup
  void release();

  // C++
  const util::WeakPtr<ThreadGroup>& weak_thread_group() const { return weak_thread_group_; }

  const fbl::RefPtr<Kernel>& kernel() const { return kernel_; }
  fbl::RefPtr<Kernel>& kernel() { return kernel_; }

  const KernelHandle<ProcessDispatcher>& process() const { return process_; }
  pid_t leader() const { return leader_; }

  // WAVL-tree Index
  pid_t GetKey() const { return leader_; }

  ~ThreadGroup();

 private:
  ThreadGroup(
      fbl::RefPtr<Kernel> kernel, KernelHandle<ProcessDispatcher> process,
      ktl::optional<starnix_sync::RwLock<ThreadGroupMutableState>::RwLockWriteGuard>& parent,
      pid_t leader, fbl::RefPtr<ProcessGroup> process_group);
};

}  // namespace starnix

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_THREAD_GROUP_H_
