// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/task/process_group.h"

#include <lib/mistos/starnix/kernel/signals/types.h>
#include <lib/mistos/starnix/kernel/task/session.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>
#include <lib/mistos/util/weak_wrapper.h>
#include <zircon/errors.h>

#include <kernel/mutex.h>
#include <ktl/optional.h>
#include <object/job_dispatcher.h>

#include <ktl/enforce.h>

namespace starnix {

fbl::Vector<fbl::RefPtr<ThreadGroup>> ProcessGroupMutableState::thread_groups() const {
  fbl::Vector<fbl::RefPtr<ThreadGroup>> thread_groups_vec;
  // fbl::AllocChecker ac;
  //  for (const auto weak_tg : thread_groups_) {
  //  if (auto strong_tg = weak_tg.CopyPointer()) {
  //     thread_groups_vec.push_back(ktl::move(strong_tg), &ac);
  //     ZX_ASSERT(ac.check());
  // }
  // }
  return thread_groups_vec;
}

bool ProcessGroupMutableState::remove(fbl::RefPtr<ThreadGroup> thread_group) {
  auto it = thread_groups_.find(thread_group->leader());
  if (it != thread_groups_.end()) {
    thread_groups_.erase(it);
  }
  return thread_groups_.is_empty();
}

ProcessGroup::~ProcessGroup() { mutable_state_.Write()->thread_groups_.clear(); }

ProcessGroup::ProcessGroup(fbl::RefPtr<Session> session, pid_t leader)
    : session_(ktl::move(session)), leader_(leader) {}

fbl::RefPtr<ProcessGroup> ProcessGroup::New(pid_t leader,
                                            ktl::optional<fbl::RefPtr<Session>> session) {
  auto s = session.has_value() ? session.value() : Session::New(leader);

  fbl::AllocChecker ac;
  fbl::RefPtr<ProcessGroup> pg = fbl::AdoptRef(new (&ac) ProcessGroup(s, leader));
  ASSERT(ac.check());

  return ktl::move(pg);
}

void ProcessGroup::insert(fbl::RefPtr<ThreadGroup> thread_group) {
  mutable_state_.Write()->thread_groups_.insert(util::WeakPtr<ThreadGroup>(thread_group.get()));
}

bool ProcessGroup::remove(fbl::RefPtr<ThreadGroup> thread_group) {
  return mutable_state_.Write()->remove(thread_group);
}

// void ProcessGroup::send_signals(const fbl::Vector<Signal>& signals) {}

void ProcessGroup::check_orphaned() {}

const fbl::RefPtr<Session>& ProcessGroup::session() const { return session_; }

}  // namespace starnix
