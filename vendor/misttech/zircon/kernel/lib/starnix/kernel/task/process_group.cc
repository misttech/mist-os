// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/task/process_group.h"

#include <lib/mistos/starnix/kernel/signals/types.h>
#include <lib/mistos/starnix/kernel/task/session.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>
#include <lib/mistos/util/weak_wrapper.h>
#include <zircon/assert.h>
#include <zircon/errors.h>

#include <fbl/alloc_checker.h>
#include <kernel/mutex.h>
#include <ktl/optional.h>
#include <object/job_dispatcher.h>

#include <ktl/enforce.h>

namespace starnix {

ProcessGroupMutableState::ProcessGroupMutableState() = default;

fbl::Vector<fbl::RefPtr<ThreadGroup>> ProcessGroupMutableState::thread_groups() const {
  fbl::Vector<fbl::RefPtr<ThreadGroup>> thread_groups_vec;
  fbl::AllocChecker ac;
  for (auto it = thread_groups_.begin(); it != thread_groups_.end(); ++it) {
    if (auto strong_tg = it.CopyPointer().Lock()) {
      thread_groups_vec.push_back(ktl::move(strong_tg), &ac);
      ZX_ASSERT(ac.check());
    } else {
      ZX_ASSERT_MSG(strong_tg,
                    "Weak references to thread_groups in ProcessGroup must always be valid");
    }
  }
  return ktl::move(thread_groups_vec);
}

bool ProcessGroupMutableState::remove(fbl::RefPtr<ThreadGroup> thread_group) {
  auto it = thread_groups_.find(thread_group->leader_);
  if (it != thread_groups_.end()) {
    thread_groups_.erase(it);
  }
  return thread_groups_.is_empty();
}

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

ProcessGroup::~ProcessGroup() = default;

void ProcessGroup::insert(fbl::RefPtr<ThreadGroup> thread_group) {
  mutable_state_.Write()->thread_groups_.insert(thread_group->weak_thread_group_);
}

bool ProcessGroup::remove(fbl::RefPtr<ThreadGroup> thread_group) {
  return mutable_state_.Write()->remove(thread_group);
}

void ProcessGroup::send_signals(const fbl::Vector<starnix_uapi::Signal>& signals) const {
  auto thread_groups = Read()->thread_groups();
  send_signals_to_thread_groups(signals, thread_groups);
}

void ProcessGroup::check_orphaned() const {
  // Get thread groups while holding read lock
  fbl::Vector<fbl::RefPtr<ThreadGroup>> thread_groups;
  {
    auto state = Read();
    if (state->orphaned_) {
      return;
    }
    thread_groups = state->thread_groups();
  }

  // Check if any parent is in same session but different process group
  for (const auto& tg : thread_groups) {
    auto parent = tg->Read()->parent_;
    if (!parent.has_value()) {
      return;
    }

    auto parent_tg = parent.value().upgrade();
    if (!parent_tg) {
      continue;
    }

    auto parent_state = parent_tg->Read();
    if (parent_state->process_group_.get() != this &&
        parent_state->process_group_->session_ == session_) {
      return;
    }
  }

  // Get thread groups again after marking orphaned
  {
    auto state = Write();
    if (state->orphaned_) {
      return;
    }
    state->orphaned_ = true;
    thread_groups = state->thread_groups();
  }

  // Check if any thread group is stopped and send signals
  bool has_stopped = false;
  for (const auto& tg : thread_groups) {
    if (StopStateHelper::is_stopping_or_stopped(tg->load_stopped())) {
      has_stopped = true;
      break;
    }
  }

  if (has_stopped) {
    fbl::AllocChecker ac;
    fbl::Vector<starnix_uapi::Signal> signals;
    signals.push_back(starnix_uapi::kSIGHUP, &ac);
    ZX_ASSERT(ac.check());
    signals.push_back(starnix_uapi::kSIGCONT, &ac);
    ZX_ASSERT(ac.check());
    send_signals_to_thread_groups(signals, thread_groups);
  }
}

void ProcessGroup::send_signals_to_thread_groups(
    const fbl::Vector<starnix_uapi::Signal>& signals,
    const fbl::Vector<fbl::RefPtr<ThreadGroup>>& thread_groups) {
  for (const auto& thread_group : thread_groups) {
    for (const auto& signal : signals) {
      thread_group->Write()->send_signal(SignalInfo::Default(signal));
    }
  }
}

}  // namespace starnix
