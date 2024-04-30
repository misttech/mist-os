// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/task/process_group.h"

#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>
#include <zircon/errors.h>

#include <kernel/mutex.h>

namespace starnix {

ProcessGroupMutableState::ProcessGroupMutableState() = default;

bool ProcessGroupMutableState::Initialize() { return true; }

ProcessGroup::ProcessGroup(fbl::RefPtr<Session> session, pid_t leader)
    : session_(std::move(session)), leader_(leader) {}

ProcessGroup::~ProcessGroup() {}

zx_status_t ProcessGroup::New(pid_t pid, fbl::RefPtr<Session> session,
                              fbl::RefPtr<ProcessGroup>* out) {
  fbl::AllocChecker ac;

  fbl::RefPtr<ProcessGroup> pg = fbl::AdoptRef(new (&ac) ProcessGroup(session, pid));
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  *out = std::move(pg);
  return ZX_OK;
}

void ProcessGroup::insert(fbl::RefPtr<ThreadGroup> thread_group) {
  Guard<Mutex> lock(&pg_mutable_state_rw_lock_);
  mutable_state_.thread_groups().insert(thread_group);
}

}  // namespace starnix
