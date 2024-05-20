// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/task/process_group.h"

#include <lib/mistos/starnix/kernel/task/session.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>
#include <zircon/errors.h>

#include <kernel/mutex.h>
#include <ktl/optional.h>

#include <ktl/enforce.h>

namespace starnix {

ProcessGroupMutableState::ProcessGroupMutableState() = default;

bool ProcessGroupMutableState::Initialize() { return true; }

ProcessGroup::~ProcessGroup() = default;

ProcessGroup::ProcessGroup(fbl::RefPtr<Session> session, pid_t _leader)
    : session(ktl::move(session)), leader(_leader) {}

fbl::RefPtr<ProcessGroup> ProcessGroup::New(pid_t _leader,
                                            ktl::optional<fbl::RefPtr<Session>> _session) {
  auto session = _session.has_value() ? *_session : Session::New(_leader);

  fbl::AllocChecker ac;
  fbl::RefPtr<ProcessGroup> pg = fbl::AdoptRef(new (&ac) ProcessGroup(session, _leader));
  ASSERT(ac.check());

  return ktl::move(pg);
  ;
}

void ProcessGroup::insert(fbl::RefPtr<ThreadGroup> thread_group) {
  Guard<Mutex> lock(&pg_mutable_state_rw_lock_);
  mutable_state_.thread_groups().insert(thread_group);
}

}  // namespace starnix
