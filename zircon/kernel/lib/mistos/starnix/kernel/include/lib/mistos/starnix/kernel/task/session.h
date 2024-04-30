// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_SESSION_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_SESSION_H_

#include <lib/mistos/linux_uapi/typedefs.h>
#include <lib/mistos/starnix/kernel/task/forward.h>

#include <fbl/canary.h>
#include <fbl/intrusive_wavl_tree.h>
#include <fbl/recycler.h>
#include <fbl/ref_counted.h>
#include <fbl/ref_ptr.h>
#include <kernel/mutex.h>

namespace starnix {

class SessionMutableState {
 public:
  SessionMutableState(const SessionMutableState&) = delete;
  SessionMutableState& operator=(const SessionMutableState&) = delete;

  SessionMutableState();

  bool Initialize();

 private:
  /// The process groups in the session
  ///
  /// The references to ProcessGroup is weak to prevent cycles as ProcessGroup have a Arc reference
  /// to their session. It is still expected that these weak references are always valid, as process
  /// groups must unregister themselves before they are deleted.
  // process_groups: BTreeMap<pid_t, Weak<ProcessGroup>>,
  fbl::WAVLTree<pid_t, fbl::RefPtr<ProcessGroup>> process_groups_;

  /// The controlling terminal of the session.
  // pub controlling_terminal: Option<ControllingTerminal>,
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
  pid_t leader() const { return leader_; }

  static zx_status_t Create(pid_t leader, fbl::RefPtr<Session>* out);

 private:
  Session(pid_t leader) : leader_(leader) {}

  fbl::Canary<fbl::magic("SESS")> canary_;

  // The leader of the session
  pid_t leader_;

  DECLARE_MUTEX(SessionMutableState) session_mutable_state_rw_lock_;
  // The mutable state of the Session.
  SessionMutableState mutable_state_ TA_GUARDED(session_mutable_state_rw_lock_);
};

}  // namespace starnix

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_SESSION_H_
