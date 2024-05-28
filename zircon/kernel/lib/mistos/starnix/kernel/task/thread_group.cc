// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
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

ThreadGroupMutableState::ThreadGroupMutableState(ThreadGroup* base,
                                                 ktl::optional<fbl::RefPtr<ThreadGroup>> _parent,
                                                 fbl::RefPtr<ProcessGroup> _process_group)
    : parent(ktl::move(_parent)), process_group(ktl::move(_process_group)), base_(base) {}

pid_t ThreadGroupMutableState::leader() const { return base_->leader; }

pid_t ThreadGroupMutableState::get_ppid() const {
  if (parent.has_value()) {
    return parent.value()->leader;
  }
  return leader();
}

fbl::RefPtr<ThreadGroup> ThreadGroup::New(fbl::RefPtr<Kernel> kernel, zx::process process,
                                          ktl::optional<fbl::RefPtr<ThreadGroup>> parent,
                                          pid_t leader, fbl::RefPtr<ProcessGroup> process_group) {
  fbl::AllocChecker ac;
  fbl::RefPtr<ThreadGroup> thread_group = fbl::AdoptRef(
      new (&ac) ThreadGroup(kernel, ktl::move(process), leader, parent, process_group));
  ASSERT(ac.check());

  if (parent.has_value()) {
    //  parent.value()->mutable_state_->children().insert()
  }
  return ktl::move(thread_group);
}

uint64_t ThreadGroup::get_rlimit(starnix_uapi::Resource resource) const {
  return limits.Lock()->get(resource).rlim_cur;
}

ThreadGroup::~ThreadGroup() = default;

ThreadGroup::ThreadGroup(fbl::RefPtr<Kernel> _kernel, zx::process _process, pid_t _leader,
                         ktl::optional<fbl::RefPtr<ThreadGroup>> parent,
                         fbl::RefPtr<ProcessGroup> process_group)
    : kernel(ktl::move(_kernel)), process(ktl::move(_process)), leader(_leader) {
  if (parent.has_value()) {
    *limits.Lock() = *parent.value()->limits.Lock();
  }
  *mutable_state_.Write() = ktl::move(ThreadGroupMutableState(this, parent, process_group));
}

fit::result<Errno> ThreadGroup::add(fbl::RefPtr<Task> task) {
  auto& state = this->write();
  if (state.terminating) {
    return fit::error(errno(EINVAL));
  }

  state.tasks_.insert(TaskContainer::From(task));
  return fit::ok();
}

}  // namespace starnix
