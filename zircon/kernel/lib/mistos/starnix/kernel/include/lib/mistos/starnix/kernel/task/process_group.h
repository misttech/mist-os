// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_PROCESS_GROUP_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_PROCESS_GROUP_H_

#include <lib/mistos/linux_uapi/typedefs.h>
#include <lib/mistos/starnix/kernel/task/forward.h>
#include <lib/mistos/starnix/kernel/task/kernel.h>
#include <lib/mistos/starnix/kernel/task/session.h>
#include <lib/mistos/zx/process.h>

#include <optional>
#include <utility>

#include <fbl/canary.h>
#include <fbl/intrusive_wavl_tree.h>
#include <fbl/ref_counted.h>
#include <fbl/ref_ptr.h>

namespace starnix {

class ProcessGroupMutableState {
 public:
  using BTree = fbl::WAVLTree<pid_t, fbl::RefPtr<ThreadGroup>>;

  ProcessGroupMutableState(const ProcessGroupMutableState&) = delete;
  ProcessGroupMutableState& operator=(const ProcessGroupMutableState&) = delete;

  ProcessGroupMutableState();

  bool Initialize();

  BTree& thread_groups() { return thread_groups_; }

 private:
  /// The thread_groups in the process group.
  ///
  /// The references to ThreadGroup is weak to prevent cycles as ThreadGroup have a Arc
  /// reference to their process group. It is still expected that these weak references are
  /// always valid, as thread groups must unregister themselves before they are deleted.
  BTree thread_groups_;

  // Whether this process group is orphaned and already notified its members.
  // bool orphaned_ = false;
};

class ProcessGroup : public fbl::RefCounted<ProcessGroup>,
                     public fbl::WAVLTreeContainable<fbl::RefPtr<ProcessGroup>> {
 public:
  ~ProcessGroup();
  static zx_status_t New(pid_t pid, fbl::RefPtr<Session>, fbl::RefPtr<ProcessGroup>* out);

  pid_t leader() const { return leader_; }

  Lock<Mutex>* pg_mutable_state_rw_lock() const TA_RET_CAP(pg_mutable_state_rw_lock_) {
    return &pg_mutable_state_rw_lock_;
  }

  void insert(fbl::RefPtr<ThreadGroup> thread_group);

 private:
  ProcessGroup(fbl::RefPtr<Session> session, pid_t leader);

  fbl::Canary<fbl::magic("PGRP")> canary_;

  // The session of the process group.
  fbl::RefPtr<Session> session_;

  // The leader of the process group.
  pid_t leader_;

  /// The mutable state of the ProcessGroup.
  // mutable_state : OrderedRwLock<ProcessGroupMutableState, ProcessGroupState>,
  mutable DECLARE_MUTEX(ProcessGroup) pg_mutable_state_rw_lock_;
  ProcessGroupMutableState mutable_state_ TA_GUARDED(pg_mutable_state_rw_lock_);
};

}  // namespace starnix

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_PROCESS_GROUP_H_
