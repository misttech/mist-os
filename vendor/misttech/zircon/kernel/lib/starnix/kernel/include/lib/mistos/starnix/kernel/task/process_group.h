// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_PROCESS_GROUP_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_PROCESS_GROUP_H_

#include <lib/mistos/linux_uapi/typedefs.h>
#include <lib/mistos/util/weak_wrapper.h>
#include <lib/starnix_sync/locks.h>

#include <utility>

#include <fbl/intrusive_wavl_tree.h>
#include <fbl/ref_counted.h>
#include <fbl/ref_ptr.h>
#include <fbl/vector.h>
#include <ktl/optional.h>

class JobDispatcher;

namespace starnix {

class ThreadGroup;

namespace internal {
struct ProcessGroupTag;
}  // namespace internal

class ProcessGroupMutableState {
 private:
  using BTreeMapThreadGroup =
      fbl::TaggedWAVLTree<pid_t, util::WeakPtr<ThreadGroup>, internal::ProcessGroupTag>;

  /// The thread_groups in the process group.
  ///
  /// The references to ThreadGroup is weak to prevent cycles as ThreadGroup have a Arc
  /// reference to their process group. It is still expected that these weak references are
  /// always valid, as thread groups must unregister themselves before they are deleted.
  BTreeMapThreadGroup thread_groups_;

  // Whether this process group is orphaned and already notified its members.
  bool orphaned_ = false;

  // impl ProcessGroupMutableState<Base = ProcessGroup>
 public:
  fbl::Vector<fbl::RefPtr<ThreadGroup>> thread_groups() const;

  /// Removes the thread group from the process group. Returns whether the process group is empty.
  bool remove(fbl::RefPtr<ThreadGroup> thread_group);

  ProcessGroupMutableState();

 private:
  friend class ProcessGroup;
};

class Session;

/// A process group is a set of processes that are considered to be a unit for the purposes of job
/// control and signal delivery. Each process in a process group has the same process group
/// ID (PGID). The process with the same PID as the PGID is called the process group leader.
///
/// When a signal is sent to a process group, it is delivered to all processes in the group,
/// including the process group leader. This allows a single signal to be used to control all
/// processes in a group, such as stopping or resuming them all.
///
/// Process groups are also used for job control. The foreground and background process groups of a
/// terminal are used to determine which processes can read from and write to the terminal. The
/// foreground process group is the only process group that can read from and write to the terminal
/// at any given time.
///
/// When a process forks from its parent, the child process inherits the parent's PGID. A process
/// can also explicitly change its own PGID using the setpgid() system call.
///
/// Process groups are destroyed when the last process in the group exits.
class ProcessGroup : public fbl::RefCountedUpgradeable<ProcessGroup>,
                     public fbl::WAVLTreeContainable<util::WeakPtr<ProcessGroup>> {
 private:
  // The session of the process group.
  fbl::RefPtr<Session> session_;

  // The leader of the process group.
  pid_t leader_;

  /// The mutable state of the ProcessGroup.
  mutable starnix_sync::RwLock<ProcessGroupMutableState> mutable_state_;

 public:
  /// impl ProcessGroup
  static fbl::RefPtr<ProcessGroup> New(pid_t pid, ktl::optional<fbl::RefPtr<Session>>);

  // ordered_state_accessor!(ProcessGroup, mutable_state, ProcessGroupState);
  starnix_sync::RwLock<ProcessGroupMutableState>::RwLockReadGuard Read() const {
    return mutable_state_.Read();
  }

  starnix_sync::RwLock<ProcessGroupMutableState>::RwLockWriteGuard Write() const {
    return mutable_state_.Write();
  }

  void insert(fbl::RefPtr<ThreadGroup> thread_group);

  /// Removes the thread group from the process group. Returns whether the process group is empty.
  bool remove(fbl::RefPtr<ThreadGroup> thread_group);

  // void send_signals(const fbl::Vector<Signal>& signals);

  /// Check whether the process group became orphaned. If this is the case, send signals to its
  /// members if at least one is stopped.
  void check_orphaned();

  // C++
  const fbl::RefPtr<Session>& session() const;

  // Accessor for the leader
  pid_t leader() const { return leader_; }

  // WAVL-tree Index
  pid_t GetKey() const { return leader_; }

  ~ProcessGroup();

 private:
  ProcessGroup(fbl::RefPtr<Session> session, pid_t leader);
};

}  // namespace starnix

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_PROCESS_GROUP_H_
