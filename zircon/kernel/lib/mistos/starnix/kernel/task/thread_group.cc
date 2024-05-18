// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/task/thread_group.h"

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/task/kernel.h>
#include <lib/mistos/starnix/kernel/task/process_group.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/zx/process.h>
#include <zircon/errors.h>

#include <utility>

#include <fbl/ref_ptr.h>
#include <kernel/mutex.h>

#include <linux/errno.h>

namespace starnix {

ThreadGroupMutableState::ThreadGroupMutableState() = default;

bool ThreadGroupMutableState::Initialize(std::optional<fbl::RefPtr<ThreadGroup>> parent,
                                         fbl::RefPtr<ProcessGroup> process_group) {
  parent_ = parent;
  process_group_ = process_group;
  return true;
}

uint64_t ThreadGroup::get_rlimit(starnix_uapi::Resource resource) const {
  return limits_.Lock()->get(resource).rlim_cur;
}

pid_t ThreadGroup::get_ppid() {
  if (mutable_state_.parent_.has_value()) {
    return mutable_state_.parent_.value()->leader();
  }
  return leader();
}

zx_status_t ThreadGroup::New(fbl::RefPtr<Kernel> kernel, zx::process process,
                             std::optional<fbl::RefPtr<ThreadGroup>> parent, pid_t leader,
                             fbl::RefPtr<ProcessGroup> process_group,
                             fbl::RefPtr<ThreadGroup>* out) {
  fbl::AllocChecker ac;
  fbl::RefPtr<ThreadGroup> thread_group =
      fbl::AdoptRef(new (&ac) ThreadGroup(kernel, std::move(process), leader));
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  {
    Guard<Mutex> lock(thread_group->tg_rw_lock());
    thread_group->mutable_state_.Initialize(parent, process_group);
  }

  if (parent.has_value()) {
    //  parent.value()->mutable_state_->children().insert()
  }

  *out = std::move(thread_group);
  return ZX_OK;
}

ThreadGroup::ThreadGroup(fbl::RefPtr<Kernel> kernel, zx::process process, pid_t leader)
    : kernel_(std::move(kernel)), process_(std::move(process)), leader_(leader) {}

fit::result<Errno> ThreadGroup::add(fbl::RefPtr<Task> task) {
  Guard<Mutex> lock(&tg_rw_lock_);
  if (mutable_state_.terminating()) {
    return fit::error(errno(EINVAL));
  }

  fbl::AllocChecker ac;
  mutable_state_.tasks().insert(
      fbl::MakeRefCountedChecked<TaskContainer>(&ac, task->persistent_info()));
  if (!ac.check()) {
    return fit::error(errno(ENOMEM));
  }
  return fit::ok();
}

}  // namespace starnix
