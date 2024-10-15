// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/task/thread_group.h"

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/signals/syscalls.h>
#include <lib/mistos/starnix/kernel/task/kernel.h>
#include <lib/mistos/starnix/kernel/task/process_group.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/util/weak_wrapper.h>
#include <lib/starnix_sync/locks.h>
#include <zircon/assert.h>
#include <zircon/errors.h>

#include <fbl/alloc_checker.h>
#include <fbl/ref_ptr.h>
#include <fbl/vector.h>
#include <kernel/mutex.h>
#include <ktl/optional.h>
#include <object/process_dispatcher.h>

#include <linux/errno.h>

namespace starnix {

ThreadGroupMutableState::ThreadGroupMutableState() = default;

ThreadGroupMutableState::ThreadGroupMutableState(ThreadGroup* base,
                                                 ktl::optional<fbl::RefPtr<ThreadGroup>> _parent,
                                                 fbl::RefPtr<ProcessGroup> _process_group)
    : parent(ktl::move(_parent)), process_group(ktl::move(_process_group)), base_(base) {}

pid_t ThreadGroupMutableState::leader() const { return base_->leader(); }

fbl::Vector<fbl::RefPtr<ThreadGroup>> ThreadGroupMutableState::children() const {
  fbl::Vector<fbl::RefPtr<ThreadGroup>> children_vec;
  fbl::AllocChecker ac;
  for (auto iter = children_.begin(); iter != children_.end(); ++iter) {
    auto strong = util::WeakPtr<ThreadGroup>(iter.CopyPointer()).Lock();
    ZX_ASSERT_MSG(strong, "Weak references to processes in ThreadGroup must always be valid");
    children_vec.push_back(strong, &ac);
    ZX_ASSERT(ac.check());
  }
  return ktl::move(children_vec);
}

fbl::Vector<fbl::RefPtr<Task>> ThreadGroupMutableState::tasks() const {
  fbl::Vector<fbl::RefPtr<Task>> tasks_vec;
  fbl::AllocChecker ac;
  for (auto& tc : tasks_) {
    auto strong = tc.upgrade();
    if (strong.has_value()) {
      tasks_vec.push_back(strong.value(), &ac);
      ZX_ASSERT(ac.check());
    }
  }
  return ktl::move(tasks_vec);
}

fbl::Vector<pid_t> ThreadGroupMutableState::task_ids() const {
  fbl::Vector<pid_t> ids;
  fbl::AllocChecker ac;
  for (auto& tc : tasks_) {
    ids.push_back(tc.GetKey(), &ac);
    ZX_ASSERT(ac.check());
  }
  return ids;
}

bool ThreadGroupMutableState::contains_task(pid_t tid) const {
  return tasks_.find(tid) != tasks_.end();
}

fbl::RefPtr<Task> ThreadGroupMutableState::get_task(pid_t tid) const {
  auto it = tasks_.find(tid);
  if (it != tasks_.end()) {
    auto task = it->upgrade();
    if (task.has_value()) {
      return task.value();
    }
  }
  return nullptr;
}

pid_t ThreadGroupMutableState::get_ppid() const {
  if (parent.has_value()) {
    return parent.value()->leader();
  }
  return leader();
}

bool ThreadGroupMutableState::is_waitable() const {
  // return last_signal.has_value() && base_->load_stopped() != StopState::InProgress;
  return false;
}

WaitableChildResult ThreadGroupMutableState::get_waitable_running_children(
    ProcessSelector selector, const WaitingOptions& options, const PidTable& pids) const {
  // Implementation details omitted for brevity
  // This would contain the logic to find and return waitable running children
  // based on the given selector and options

  return WaitableChildResult::ShouldWait();
}

WaitableChildResult ThreadGroupMutableState::get_waitable_child(ProcessSelector selector,
                                                                const WaitingOptions& options,
                                                                PidTable& pids) {
  return get_waitable_running_children(selector, options, pids);
}

fbl::RefPtr<ThreadGroup> ThreadGroup::New(
    fbl::RefPtr<Kernel> _kernel, KernelHandle<ProcessDispatcher> _process,
    ktl::optional<starnix_sync::RwLock<ThreadGroupMutableState>::RwLockWriteGuard> parent,
    pid_t _leader, fbl::RefPtr<ProcessGroup> process_group) {
  fbl::AllocChecker ac;
  fbl::RefPtr<ThreadGroup> thread_group =
      fbl::AdoptRef(new (&ac) ThreadGroup(ktl::move(_kernel), ktl::move(_process), _leader,
                                          ktl::move(parent), ktl::move(process_group)));
  ASSERT(ac.check());

  if (parent) {
    //  parent.value()->mutable_state_->children().insert()
  }
  return ktl::move(thread_group);
}

void ThreadGroup::exit(ExitStatus exit_status, ktl::optional<CurrentTask> current_task) {
  if (current_task.has_value()) {
    // current_task
    //             .ptrace_event(PtraceOptions::TRACEEXIT, exit_status.signal_info_status() as u64);
  }

  auto pids = kernel_->pids.Write();
  auto state = mutable_state_.Write();
  if (state->terminating) {
    // The thread group is already terminating and all threads in the thread group have
    // already been interrupted.
    return;
  }
  state->terminating = true;

  // Drop ptrace zombies
  // state.zombie_ptracees.release(&mut pids);

  // Interrupt each task. Unlock the group because send_signal will lock the group in order
  // to call set_stopped.
  // SAFETY: tasks is kept on the stack. The static is required to ensure the lock on
  // ThreadGroup can be dropped.
  auto tasks = state->tasks();
  state.~RwLockGuard();

  // Detach from any ptraced tasks.
  // let tracees = self.ptracees.lock().keys().cloned().collect::<Vec<_>>();
  // for tracee in tracees {
  //    if let Some(task_ref) = pids.get_task(tracee).clone().upgrade() {
  //        let _ = ptrace_detach(self, task_ref.as_ref(), &UserAddress::NULL);
  //    }
  //}
  for (auto task : tasks) {
    // task->mutable_state_.Write()
    // task->thread.Write()->
  }
}

uint64_t ThreadGroup::get_rlimit(starnix_uapi::Resource resource) const {
  return limits.Lock()->get(resource).rlim_cur;
}

ThreadGroup::~ThreadGroup() = default;

ThreadGroup::ThreadGroup(
    fbl::RefPtr<Kernel> _kernel, KernelHandle<ProcessDispatcher> process, pid_t leader,
    ktl::optional<starnix_sync::RwLock<ThreadGroupMutableState>::RwLockWriteGuard> parent,
    fbl::RefPtr<ProcessGroup> process_group)
    : kernel_(ktl::move(_kernel)), process_(ktl::move(process)), leader_(leader) {
  ktl::optional<fbl::RefPtr<ThreadGroup>> ptg;
  if (parent.has_value()) {
    *limits.Lock() = *(*parent)->base_->limits.Lock();
    ptg = (*parent)->parent;
  }
  *mutable_state_.Write() = ktl::move(ThreadGroupMutableState(this, ptg, ktl::move(process_group)));
}

fit::result<Errno> ThreadGroup::add(fbl::RefPtr<Task> task) {
  auto state = this->write();
  if (state->terminating) {
    return fit::error(errno(EINVAL));
  }

  state->tasks_.insert(TaskContainer::From(ktl::move(task)));
  return fit::ok();
}

bool ProcessSelector::DoMatch(pid_t pid, const PidTable& pid_table) const {
  return ktl::visit(ProcessSelector::overloaded{
                        [](Any) { return true; }, [pid](Pid p) { return p.value == pid; },
                        [pid, &pid_table](Pgid pg) {
                          auto task_ref = pid_table.get_task(pid).Lock();
                          if (task_ref) {
                            if (auto group = pid_table.get_process_group(pg.value)) {
                              if (group.has_value()) {
                                return group == task_ref->thread_group->read()->process_group;
                              }
                            }
                          }
                          return false;
                        }},
                    selector_);
}

}  // namespace starnix
