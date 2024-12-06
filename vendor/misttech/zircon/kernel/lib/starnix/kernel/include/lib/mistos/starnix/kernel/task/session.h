// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_SESSION_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_SESSION_H_

#include <lib/mistos/linux_uapi/typedefs.h>
#include <lib/mistos/memory/weak_ptr.h>
#include <lib/mistos/starnix/kernel/task/internal/tag.h>
#include <lib/starnix_sync/locks.h>

#include <fbl/canary.h>
#include <fbl/intrusive_wavl_tree.h>
#include <fbl/recycler.h>
#include <fbl/ref_counted.h>
#include <fbl/ref_ptr.h>
#include <kernel/mutex.h>

namespace starnix {

class ProcessGroup;

struct SessionMutableState {
 private:
  /// The process groups in the session
  ///
  /// The references to ProcessGroup is weak to prevent cycles as ProcessGroup have a Arc reference
  /// to their session. It is still expected that these weak references are always valid, as process
  /// groups must unregister themselves before they are deleted.
  fbl::TaggedWAVLTree<pid_t, mtl::WeakPtr<ProcessGroup>, internal::SessionTag> process_groups_;

  /// The leader of the foreground process group. This is necessary because the leader must
  /// be returned even if the process group has already been deleted.
  // pid_t foreground_process_group;

  /// The controlling terminal of the session.
  // pub controlling_terminal: Option<ControllingTerminal>,

 public:
  /// Removes the process group from the session. Returns whether the session is empty.
  void remove(pid_t pid);

  void insert(fbl::RefPtr<ProcessGroup> process_group);

  ~SessionMutableState();
};

/// A session is a collection of `ProcessGroup` objects that are related to each other. Each
/// session has a session ID (`sid`), which is a unique identifier for the session.
///
/// The session leader is the first `ProcessGroup` in a session. It is responsible for managing the
/// session, including sending signals to all processes in the session and controlling the
/// foreground and background process groups.
///
/// When a `ProcessGroup` is created, it is automatically added to the session of its parent.
/// See `setsid(2)` for information about creating sessions.
///
/// A session can be destroyed when the session leader exits or when all process groups in the
/// session are destroyed.
class Session : public fbl::RefCounted<Session> {
 public:
  /// The leader of the session
  pid_t leader_;

 private:
  // The mutable state of the Session.
  mutable starnix_sync::RwLock<SessionMutableState> mutable_state_;

 public:
  /// impl Session
  static fbl::RefPtr<Session> New(pid_t leader);

  // ordered_state_accessor;
  starnix_sync::RwLock<SessionMutableState>::RwLockReadGuard Read() const {
    return mutable_state_.Read();
  }

  starnix_sync::RwLock<SessionMutableState>::RwLockWriteGuard Write() const {
    return mutable_state_.Write();
  }

  // C++
 public:
  ~Session();

 private:
  Session(pid_t leader);
};

}  // namespace starnix

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_SESSION_H_
