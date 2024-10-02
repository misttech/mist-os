// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_THREAD_GROUP_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_THREAD_GROUP_H_

#include <lib/fit/result.h>
#include <lib/mistos/linux_uapi/typedefs.h>
#include <lib/mistos/starnix/kernel/task/zombie_process.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/mistos/starnix_uapi/resource_limits.h>
#include <lib/mistos/util/weak_wrapper.h>
#include <lib/starnix_sync/locks.h>

#include <fbl/ref_counted.h>
#include <fbl/ref_ptr.h>
#include <ktl/optional.h>
#include <object/handle.h>

class ProcessDispatcher;

namespace starnix {

class TaskContainer;
class Kernel;
class ProcessGroup;
class ThreadGroup;
class Task;

/// The mutable state of the ThreadGroup.
class ThreadGroupMutableState {
 public:
  using BTreeMapTaskContainer = fbl::WAVLTree<pid_t, ktl::unique_ptr<TaskContainer>>;
  using BTreeMapThreadGroup = fbl::WAVLTree<pid_t, util::WeakPtr<ThreadGroup>>;

  // The parent thread group.
  //
  // The value needs to be writable so that it can be re-parent to the correct subreaper if the
  // parent ends before the child.
  ktl::optional<fbl::RefPtr<ThreadGroup>> parent;

 private:
  // The tasks in the thread group.
  //
  // The references to Task is weak to prevent cycles as Task have a Arc reference to their
  // thread group.
  // It is still expected that these weak references are always valid, as tasks must unregister
  // themselves before they are deleted.
  BTreeMapTaskContainer tasks_;

 public:
  // The children of this thread group.
  //
  // The references to ThreadGroup is weak to prevent cycles as ThreadGroup have a Arc reference
  // to their parent.
  // It is still expected that these weak references are always valid, as thread groups must
  // unregister themselves before they are deleted.
  BTreeMapThreadGroup children;

  /// Child tasks that have exited, but not yet been waited for.
  // pub zombie_children: Vec<OwnedRef<ZombieProcess>>,

  /// ptracees of this process that have exited, but not yet been waited for.
  // pub zombie_ptracees: ZombiePtraces,

  // Child tasks that have exited, but the zombie ptrace needs to be consumed
  // before they can be waited for.  (pid_t, pid_t) is the original tracer and
  // tracee, so the tracer can be updated with a reaper if this thread group
  // exits.
  // pub deferred_zombie_ptracers: Vec<(pid_t, pid_t)>,

  /// WaitQueue for updates to the WaitResults of tasks in this group.
  // pub child_status_waiters: WaitQueue,

  /// Whether this thread group will inherit from children of dying processes in its descendant
  /// tree.
  // pub is_child_subreaper: bool,

  /// The IDs used to perform shell job control.
  fbl::RefPtr<ProcessGroup> process_group;

  /// The timers for this thread group (from timer_create(), etc.).
  // pub timers: TimerTable,

  bool did_exec = false;

  /// Wait queue for updates to `stopped`.
  // pub stopped_waiters: WaitQueue,

  /// A signal that indicates whether the process is going to become waitable
  /// via waitid and waitpid for either WSTOPPED or WCONTINUED, depending on
  /// the value of `stopped`. If not None, contains the SignalInfo to return.
  // pub last_signal: Option<SignalInfo>,

  // pub leader_exit_info: Option<ProcessExitInfo>,

  bool terminating = false;

  /// The SELinux operations for this thread group.
  // pub selinux_state: Option<SeLinuxThreadGroupState>,

  /// Time statistics accumulated from the children.
  // pub children_time_stats: TaskTimeStats,

  /// Personality flags set with `sys_personality()`.
  // pub personality: PersonalityFlags,

  /// Thread groups allowed to trace tasks in this this thread group.
  // pub allowed_ptracers: PtraceAllowedPtracers,

  /// impl ThreadGroupMutableState<Base = ThreadGroup, BaseType = Arc<ThreadGroup>>
  pid_t leader() const;

  pid_t get_ppid() const;

  size_t tasks_count() const { return tasks_.size(); }

  ThreadGroupMutableState();

  ThreadGroupMutableState(ThreadGroup* base, ktl::optional<fbl::RefPtr<ThreadGroup>> parent,
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
class ThreadGroup : public fbl::RefCountedUpgradeable<ThreadGroup>,
                    public fbl::WAVLTreeContainable<util::WeakPtr<ThreadGroup>> {
 public:
  /// Weak reference to the `OwnedRef` of this `ThreadGroup`. This allows to retrieve the
  /// `TempRef` from a raw `ThreadGroup`.
  util::WeakPtr<ThreadGroup> weak_thread_group;

  // The kernel to which this thread group belongs.
  fbl::RefPtr<Kernel> kernel;

  /// A handle to the underlying Zircon process object.
  ///
  /// Currently, we have a 1-to-1 mapping between thread groups and zx::process
  /// objects. This approach might break down if/when we implement CLONE_VM
  /// without CLONE_THREAD because that creates a situation where two thread
  /// groups share an address space. To implement that situation, we might
  /// need to break the 1-to-1 mapping between thread groups and zx::process
  /// or teach zx::process to share address spaces.
  KernelHandle<ProcessDispatcher> process;

  /// The lead task of this thread group.
  ///
  /// The lead task is typically the initial thread created in the thread group.
  pid_t leader;

  /// The signal actions that are registered for this process.
  // pub signal_actions: Arc<SignalActions>,

  /// A mechanism to be notified when this `ThreadGroup` is destroyed.
  // pub drop_notifier: DropNotifier,

  /// Whether the process is currently stopped.
  ///
  /// Must only be set when the `mutable_state` write lock is held.
  // stop_state: AtomicStopState,

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

  uint64_t get_rlimit(starnix_uapi::Resource resource) const;

  fit::result<Errno> add(fbl::RefPtr<Task> task);

  /// state_accessor!(ThreadGroup, mutable_state, Arc<ThreadGroup>);
  starnix_sync::RwLock<ThreadGroupMutableState>::RwLockReadGuard read() const {
    return mutable_state_.Read();
  }

  starnix_sync::RwLock<ThreadGroupMutableState>::RwLockWriteGuard write() {
    return mutable_state_.Write();
  }

  // C++
  ~ThreadGroup();

  // WAVL-tree Index
  pid_t GetKey() const { return leader; }

 private:
  ThreadGroup(fbl::RefPtr<Kernel> kernel, KernelHandle<ProcessDispatcher> process, pid_t leader,
              ktl::optional<starnix_sync::RwLock<ThreadGroupMutableState>::RwLockWriteGuard> parent,
              fbl::RefPtr<ProcessGroup> process_group);
};

}  // namespace starnix

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_THREAD_GROUP_H_
