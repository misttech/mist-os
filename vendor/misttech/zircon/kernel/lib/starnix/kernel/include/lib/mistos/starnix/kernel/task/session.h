// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_SESSION_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_SESSION_H_

#include <lib/mistos/linux_uapi/typedefs.h>
#include <lib/mistos/starnix/kernel/sync/locks.h>
#include <lib/mistos/util/weak_wrapper.h>

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
  fbl::WAVLTree<pid_t, util::WeakPtr<ProcessGroup>> process_groups_;

 public:
  /// The controlling terminal of the session.
  // pub controlling_terminal: Option<ControllingTerminal>,

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
  pid_t leader;

 private:
  // The mutable state of the Session.
  mutable RwLock<SessionMutableState> mutable_state_;

 public:
  /// impl Session
  static fbl::RefPtr<Session> New(pid_t _leader);

  // C++
 public:
  ~Session();

 private:
  Session(pid_t _leader);
};

}  // namespace starnix

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_SESSION_H_
