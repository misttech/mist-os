// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/task/thread_group.h"

#include <lib/fit/result.h>
#include <lib/mistos/memory/weak_ptr.h>
#include <lib/mistos/starnix/kernel/signals/signal_handling.h>
#include <lib/mistos/starnix/kernel/signals/syscalls.h>
#include <lib/mistos/starnix/kernel/signals/types.h>
#include <lib/mistos/starnix/kernel/task/exit_status.h>
#include <lib/mistos/starnix/kernel/task/kernel.h>
#include <lib/mistos/starnix/kernel/task/process_group.h>
#include <lib/mistos/starnix/kernel/task/session.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix_uapi/signals.h>
#include <lib/starnix_sync/locks.h>
#include <trace.h>
#include <zircon/assert.h>
#include <zircon/errors.h>

#include <limits>
#include <memory>

#include <fbl/alloc_checker.h>
#include <fbl/intrusive_hash_table.h>
#include <fbl/ref_ptr.h>
#include <fbl/vector.h>
#include <kernel/mutex.h>
#include <ktl/optional.h>
#include <object/process_dispatcher.h>

#include "../kernel_priv.h"

#include <linux/errno.h>

#define LOCAL_TRACE STARNIX_KERNEL_GLOBAL_TRACE(0)

namespace starnix {

fbl::RefPtr<ZombieProcess> ZombieProcess::New(const ThreadGroupMutableState& thread_group,
                                              const Credentials& credentials,
                                              ProcessExitInfo exit_info) {
  // TaskTimeStats time_stats = thread_group.base.TimeStats() + thread_group.children_time_stats;
  // Note: TaskTimeStats addition is commented out as it's not implemented in the C++ version
  fbl::AllocChecker ac;
  auto zp = fbl::AdoptRef(new (&ac) ZombieProcess(thread_group.base_->leader_,
                                                  thread_group.process_group_->leader_,
                                                  credentials.uid_, exit_info, true));
  ZX_ASSERT(ac.check());
  return zp;
}

void ZombieProcess::release(PidTable& pids) {
  LTRACE_ENTRY_OBJ;
  if (is_canonical) {
    pids.remove_zombie(pid);
  }
  LTRACE_EXIT_OBJ;
}

ThreadGroupMutableState::ThreadGroupMutableState() = default;

ThreadGroupMutableState::ThreadGroupMutableState(ThreadGroup* base,
                                                 ktl::optional<ThreadGroupParent> parent,
                                                 fbl::RefPtr<ProcessGroup> process_group)
    : parent_(ktl::move(parent)), process_group_(ktl::move(process_group)), base_(base) {}

pid_t ThreadGroupMutableState::leader() const { return base_->leader_; }

fbl::Vector<fbl::RefPtr<ThreadGroup>> ThreadGroupMutableState::children() const {
  fbl::Vector<fbl::RefPtr<ThreadGroup>> children_vec;
  for (auto iter = children_.begin(); iter != children_.end(); ++iter) {
    auto strong = iter.CopyPointer().Lock();
    ZX_ASSERT_MSG(strong, "Weak references to processes in ThreadGroup must always be valid");
    fbl::AllocChecker ac;
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
  if (parent_.has_value()) {
    return parent_->upgrade()->leader_;
  }
  return leader();
}

void ThreadGroupMutableState::set_process_group(const fbl::RefPtr<ProcessGroup>& process_group,
                                                PidTable& pids) {
  if (process_group_ == process_group) {
    return;
  }
  leave_process_group(pids);
  process_group_ = process_group;
  process_group->insert(fbl::RefPtr<ThreadGroup>(base_));
}

void ThreadGroupMutableState::leave_process_group(PidTable& pids) {
  LTRACE_ENTRY_OBJ;
  if (process_group_->remove(fbl::RefPtr<ThreadGroup>(base_))) {
    process_group_->session_->Write()->remove(process_group_->leader_);
    pids.remove_process_group(process_group_->leader_);
  }
  LTRACE_EXIT_OBJ;
}

bool ThreadGroupMutableState::is_waitable() const {
  return last_signal_.has_value() && !StopStateHelper::is_in_progress(base_->load_stopped());
}

ktl::optional<WaitResult> ThreadGroupMutableState::get_waitable_zombie(
    ZombieListFn zombie_list, ProcessSelector selector, const WaitingOptions& options,
    PidTable& pids) {
  // The zombies whose pid matches the pid selector queried.
  auto zombie_matches_pid_selector = [&](const fbl::RefPtr<ZombieProcess>& zombie) {
    return ktl::visit(ProcessSelector::overloaded{
                          [](ProcessSelector::Any) { return true; },
                          [&](ProcessSelector::Pid pid) { return zombie->pid == pid.value; },
                          [&](ProcessSelector::Pgid pgid) { return zombie->pgid == pgid.value; }},
                      selector.selector_);
  };

  // The zombies whose exit signal matches the waiting options queried.
  auto zombie_matches_wait_options = [&](const fbl::RefPtr<ZombieProcess>& zombie) {
    if (options.wait_for_all()) {
      return true;
    }
    return ThreadGroupMutableState::is_correct_exit_signal(options.wait_for_clone(),
                                                           zombie->exit_info.exit_signal);
  };

  // We look for the last zombie in the vector that matches pid selector and waiting options
  auto it = std::find_if(zombie_list(this).rbegin(), zombie_list(this).rend(),
                         [&](const fbl::RefPtr<ZombieProcess>& zombie) {
                           return zombie_matches_wait_options(zombie) &&
                                  zombie_matches_pid_selector(zombie);
                         });

  if (it == zombie_list(this).rend()) {
    return ktl::nullopt;
  }

  size_t position = std::distance(it, zombie_list(this).rend()) - 1;

  if (options.keep_waitable_state()) {
    return zombie_list(this)[position]->to_wait_result();
  }

  auto zombie = zombie_list(this).erase(position);
  // children_time_stats_ += zombie->time_stats;
  auto result = zombie->to_wait_result();
  // zombie->release(pids);
  return result;
}

bool ThreadGroupMutableState::is_correct_exit_signal(bool wait_for_clone,
                                                     ktl::optional<Signal> exit_signal) {
  return wait_for_clone == (exit_signal != kSIGCHLD);
}

WaitableChildResult ThreadGroupMutableState::get_waitable_running_children(
    ProcessSelector selector, const WaitingOptions& options, const PidTable& pids) const {
  // The children whose pid matches the pid selector queried.

  auto filter_children_by_pid_selector = [&](const fbl::RefPtr<ThreadGroup>& child) -> bool {
    return ktl::visit(ProcessSelector::overloaded{
                          [](ProcessSelector::Any) { return true; },
                          [&](ProcessSelector::Pid pid) { return child->leader_ == pid.value; },
                          [&](ProcessSelector::Pgid pgid) {
                            return pids.get_process_group(pgid.value) ==
                                   child->Read()->process_group_;
                          }},
                      selector.selector_);
  };

  // The children whose exit signal matches the waiting options queried.
  auto filter_children_by_waiting_options = [&](const fbl::RefPtr<ThreadGroup>& child) {
    if (options.wait_for_all()) {
      return true;
    }
    auto child_state = child->Read();
    if (child_state->terminating_) {
      // Child is terminating.  In addition to its original location,
      // the leader may have exited, and its exit signal may be in the
      // leader_exit_info.
      if (child_state->leader_exit_info_.has_value()) {
        auto& info = child_state->leader_exit_info_.value();
        if (info.exit_signal.has_value()) {
          return ThreadGroupMutableState::is_correct_exit_signal(options.wait_for_clone(),
                                                                 info.exit_signal);
        }
      }
    }

    for (const auto& task : tasks_) {
      auto info = task.info();
      if (ThreadGroupMutableState::is_correct_exit_signal(options.wait_for_clone(),
                                                          info->exit_signal())) {
        return true;
      }
    }
    return false;
  };

  // If wait_for_exited flag is disabled or no terminated children were found we look for living
  // children.
  fbl::Vector<fbl::RefPtr<ThreadGroup>> selected_children;
  for (auto it = children_.begin(); it != children_.end(); ++it) {
    auto thread_group = it.CopyPointer().Lock();
    if (thread_group && filter_children_by_pid_selector(thread_group) &&
        filter_children_by_waiting_options(thread_group)) {
      LTRACEF("Found child pid: %d\n", thread_group->leader_);
      fbl::AllocChecker ac;
      selected_children.push_back(thread_group, &ac);
      ZX_ASSERT(ac.check());
    }
  }

  if (selected_children.is_empty()) {
    /*
    // There still might be a process that ptrace hasn't looked at yet.
    if self.deferred_zombie_ptracers.iter().any(|&(_, tracee)| match selector {
        ProcessSelector::Any => true,
        ProcessSelector::Pid(pid) => tracee == pid,
        ProcessSelector::Pgid(pgid) => {
            pids.get_process_group(pgid).as_ref() == pids.get_process_group(tracee).as_ref()
        }
    }) {
        return WaitableChildResult::ShouldWait;
    }
    */
    return WaitableChildResult::NoneFound();
  }

  for (auto& child : selected_children) {
    auto c = child->Write();
    if (c->last_signal_.has_value()) {
      auto build_wait_result = [&](ThreadGroupMutableState& child,
                                   const auto& exit_status_fn) -> WaitResult {
        SignalInfo siginfo = [&options, &c]() {
          if (options.keep_waitable_state()) {
            return c->last_signal_.value();
          }
          return ktl::move(c->last_signal_).value();
        }();

        ExitStatus exit_status = [&siginfo, &exit_status_fn]() {
          if (siginfo.signal == kSIGKILL) {
            // This overrides the stop/continue choice.
            return ExitStatus::Kill(siginfo);
          }
          return exit_status_fn(siginfo);
        }();

        auto info = (*child.tasks_.begin()).info();
        return WaitResult{.pid = child.base_->leader_,
                          .uid = info->creds().uid_,
                          .exit_info = {.status = exit_status, .exit_signal = info->exit_signal()},
                          .time_stats = {}};
      };

      auto child_stopped = c->base_->load_stopped();
      if (child_stopped == StopState::Awake && options.wait_for_continued()) {
        return WaitableChildResult::ReadyNow(build_wait_result(*c, [](const SignalInfo& siginfo) {
          return ExitStatus::Continue(siginfo, PtraceEvent::None);
        }));
      }
      if (child_stopped == StopState::GroupStopped && options.wait_for_stopped()) {
        return WaitableChildResult::ReadyNow(build_wait_result(*c, [](const SignalInfo& siginfo) {
          return ExitStatus::Stop(siginfo, PtraceEvent::None);
        }));
      }
    }
  }

  return WaitableChildResult::ShouldWait();
}

WaitableChildResult ThreadGroupMutableState::get_waitable_child(ProcessSelector selector,
                                                                const WaitingOptions& options,
                                                                PidTable& pids) {
  if (options.wait_for_exited()) {
    auto waitable_zombie = get_waitable_zombie(
        [](ThreadGroupMutableState* state) -> fbl::Vector<fbl::RefPtr<ZombieProcess>>& {
          return state->zombie_children_;
        },
        selector, options, pids);
    if (waitable_zombie.has_value()) {
      return WaitableChildResult::ReadyNow(waitable_zombie.value());
    }
  }
  return get_waitable_running_children(selector, options, pids);
}

fit::result<Errno, fbl::RefPtr<Task>> ThreadGroupMutableState::get_live_task() const {
  ktl::optional<fbl::RefPtr<Task>> task;
  auto it = tasks_.find(leader());
  if (it != tasks_.end()) {
    task = it->upgrade();
  }

  if (task.has_value()) {
    return fit::ok(task.value());
  }
  // TODO (Herrera)
  // convert => .or_else(|| self.tasks().next())

  return fit::error(errno(ESRCH));
}

StopState ThreadGroupMutableState::set_stopped(StopState new_stopped,
                                               ktl::optional<SignalInfo> siginfo,
                                               bool finalize_only) {
  ktl::optional<StopState> stopped = base_->check_stopped_state(new_stopped, finalize_only);
  if (stopped.has_value()) {
    return stopped.value();
  }

  // Thread groups don't transition to group stop if they are waking, because waking
  // means something told it to wake up (like a SIGCONT) but hasn't finished yet.
  if (base_->load_stopped() == StopState::Waking &&
      (new_stopped == StopState::GroupStopping || new_stopped == StopState::GroupStopped)) {
    return base_->load_stopped();
  }

  // TODO(https://g-issues.fuchsia.dev/issues/306438676): When thread
  // group can be stopped inside user code, tasks/thread groups will
  // need to be either restarted or stopped here.
  store_stopped(new_stopped);
  if (siginfo.has_value()) {
    // We don't want waiters to think the process was unstopped
    // because of a sigkill. They will get woken when the
    // process dies.
    if (siginfo.value().signal != kSIGKILL) {
      last_signal_ = siginfo;
    }
  }
  if (new_stopped == StopState::Waking || new_stopped == StopState::ForceWaking) {
    stopped_waiters_.notify_all();
  }

  ktl::optional<ThreadGroupParent> parent_opt;
  if (!StopStateHelper::is_in_progress(new_stopped)) {
    parent_opt = parent_;
  }

  // Drop the lock before locking the parent.
  // TODO (Herrera) ??? std::destroy_at(std::addressof(this));
  if (parent_opt.has_value()) {
    auto parent = parent_opt.value().upgrade();
    if (parent) {
      parent->Write()->child_status_waiters_.notify_all();
    }
  }

  return new_stopped;
}

void ThreadGroupMutableState::store_stopped(StopState state) {
  // We don't actually use the guard but we require it to enforce that the
  // caller holds the thread group's mutable state lock (identified by
  // mutable access to the thread group's mutable state).

  base_->stop_state_.store(state, std::memory_order_relaxed);
}

void ThreadGroupMutableState::send_signal(SignalInfo signal_info) {
  auto sigaction = base_->signal_actions_->Get(signal_info.signal);
  auto action = action_for_signal(signal_info, sigaction);

  base_->pending_signals_.Lock()->enqueue(signal_info);
  fbl::Vector<mtl::WeakPtr<Task>> tasks;
  for (auto& t : tasks_) {
    fbl::AllocChecker ac;
    tasks.push_back(t.weak_clone(), &ac);
    ZX_ASSERT(ac.check());
  }

  // Set state to waking before interrupting any tasks.
  if (signal_info.signal == kSIGKILL) {
    set_stopped(StopState::ForceWaking, signal_info, false);
  } else if (signal_info.signal == kSIGCONT) {
    set_stopped(StopState::Waking, signal_info, false);
  }

  bool has_interrupted_task = false;
  for (auto& tc : tasks) {
    auto task = tc.Lock();
    auto task_state = task->Write();

    if (signal_info.signal == kSIGKILL) {
      task_state->set_stopped(StopState::ForceWaking, {}, {}, {});
    } else if (signal_info.signal == kSIGCONT) {
      task_state->set_stopped(StopState::Waking, {}, {}, {});
    }

    auto is_masked = task_state->is_signal_masked(signal_info.signal);
    auto was_masked = task_state->is_signal_masked_by_saved_mask(signal_info.signal);

    auto is_queued =
        action != DeliveryAction::Ignore || is_masked || was_masked || task_state->is_ptraced();

    if (is_queued) {
      // task_state->notify_signal_waiters();
      task_state->set_flags(TaskFlags(TaskFlagsEnum::SIGNALS_AVAILABLE), true);

      if (!is_masked && action.must_interrupt(sigaction) && !has_interrupted_task) {
        // Only interrupt one task, and only interrupt if the signal was actually queued
        // and the action must interrupt.
        task->interrupt();
        has_interrupted_task = true;
      }
    }
  }
}

fbl::RefPtr<ThreadGroup> ThreadGroup::New(
    fbl::RefPtr<Kernel> kernel, KernelHandle<ProcessDispatcher> process,
    ktl::optional<starnix_sync::RwLock<ThreadGroupMutableState>::RwLockWriteGuard> parent,
    pid_t leader, fbl::RefPtr<ProcessGroup> process_group,
    fbl::RefPtr<SignalActions> signal_actions) {
  fbl::AllocChecker ac;
  fbl::RefPtr<ThreadGroup> thread_group =
      fbl::AdoptRef(new (&ac) ThreadGroup(ktl::move(kernel), ktl::move(process), parent, leader,
                                          process_group, ktl::move(signal_actions)));
  ASSERT(ac.check());

  if (parent.has_value()) {
    // thread_group.next_seccomp_filter_id.reset(parent.base.next_seccomp_filter_id.get());
    LTRACEF("Parent ThreadGroup(%p)->insert(%p)\n", parent.value()->base_,
            thread_group->weak_thread_group_.get());
    parent.value()->children_.insert(thread_group->weak_thread_group_);
    process_group->insert(thread_group);
  }
  return ktl::move(thread_group);
}

ThreadGroup::ThreadGroup(
    fbl::RefPtr<Kernel> kernel, KernelHandle<ProcessDispatcher> process,
    ktl::optional<starnix_sync::RwLock<ThreadGroupMutableState>::RwLockWriteGuard>& parent,
    pid_t leader, fbl::RefPtr<ProcessGroup> process_group,
    fbl::RefPtr<SignalActions> signal_actions)
    : kernel_(ktl::move(kernel)),
      process_(ktl::move(process)),
      leader_(leader),
      signal_actions_(ktl::move(signal_actions)),
      stop_state_(AtomicStopState(StopState::Awake)),
      weak_factory_(this) {
  LTRACE_ENTRY_OBJ;
  ktl::optional<ThreadGroupParent> tgp;

  weak_thread_group_ = weak_factory_.GetWeakPtr();
  observer_.set_thread_group(weak_factory_.GetWeakPtr());

  if (parent.has_value()) {
    // A child process created via fork(2) inherits its parent's
    // resource limits.  Resource limits are preserved across execve(2).
    *limits.Lock() = *(*parent)->base_->limits.Lock();

    auto p = (*parent)->base_->weak_thread_group_;
    tgp = ThreadGroupParent::From(p);
  }

  *mutable_state_.Write() = ktl::move(ThreadGroupMutableState(this, tgp, ktl::move(process_group)));

  LTRACE_EXIT_OBJ;
}

ThreadGroup::~ThreadGroup() {
  LTRACE_ENTRY_OBJ;
  auto state = mutable_state_.Read();
  ZX_ASSERT(state->tasks_.is_empty());
  ZX_ASSERT(state->children_.is_empty());
  LTRACE_EXIT_OBJ;
}

void ThreadGroup::exit(ExitStatus exit_status, ktl::optional<CurrentTask> current_task) {
  LTRACE_ENTRY_OBJ;

  if (current_task.has_value()) {
    // current_task
    //             .ptrace_event(PtraceOptions::TRACEEXIT, exit_status.signal_info_status() as u64);
  }

  auto pids = kernel_->pids_.Write();
  auto state = mutable_state_.Write();
  if (state->terminating_) {
    // The thread group is already terminating and all threads in the thread group have
    // already been interrupted.
    return;
  }
  state->terminating_ = true;

  // Drop ptrace zombies
  // state.zombie_ptracees.release(&mut pids);

  // Interrupt each task. Unlock the group because send_signal will lock the group in order
  // to call set_stopped.
  // SAFETY: tasks is kept on the stack. The static is required to ensure the lock on
  // ThreadGroup can be dropped.
  auto tasks = state->tasks();
  std::destroy_at(std::addressof(state));

  // Detach from any ptraced tasks.
  // let tracees = self.ptracees.lock().keys().cloned().collect::<Vec<_>>();
  // for tracee in tracees {
  //    if let Some(task_ref) = pids.get_task(tracee).clone().upgrade() {
  //        let _ = ptrace_detach(self, task_ref.as_ref(), &UserAddress::NULL);
  //    }
  //}
  for (auto& task : tasks) {
    task->Write()->set_exit_status(exit_status);
    send_standard_signal(task, SignalInfo::Default(kSIGKILL));
  }
  LTRACE_EXIT_OBJ;
}

uint64_t ThreadGroup::get_rlimit(starnix_uapi::Resource resource) const {
  return limits.Lock()->get(resource).rlim_cur;
}

fit::result<Errno> ThreadGroup::add(fbl::RefPtr<Task> task) const {
  auto state = Write();
  if (state->terminating_) {
    return fit::error(errno(EINVAL));
  }

  state->tasks_.insert(TaskContainer::From(ktl::move(task)));
  return fit::ok();
}

void ThreadGroup::remove(fbl::RefPtr<Task> task) const {
  auto pids = kernel_->pids_.Write();

  // task->set_ptrace_zombie(pids.get());
  pids->remove_task(task->id_);

  auto state = Write();

  TaskPersistentInfo persistent_info;
  auto it = state->tasks_.find(task->id_);
  if (it != state->tasks_.end()) {
    persistent_info = it->into();
    state->tasks_.erase(it);
  } else {
    // The task has never been added. The only expected case is that this thread was
    // already terminating.
    ZX_DEBUG_ASSERT(state->terminating_);
    return;
  }

  if (task->id_ == leader_) {
    ExitStatus exit_status =
        task->exit_status().value_or(ExitStatus::Exit(std::numeric_limits<uint8_t>::max()));
    state->leader_exit_info_ = ProcessExitInfo{
        .status = exit_status,
        .exit_signal = persistent_info->Lock()->exit_signal(),
    };
  }

  if (state->tasks_.is_empty()) {
    state->terminating_ = true;

    // Replace PID table entry with a zombie.
    ZX_ASSERT_MSG(state->leader_exit_info_.has_value(), "Failed to capture leader exit status");
    auto exit_info = state->leader_exit_info_.value();
    auto zombie = ZombieProcess::New(*state, persistent_info->Lock()->creds(), exit_info);
    pids->kill_process(leader_, zombie->weak_factory_.GetWeakPtr());

    state->leave_process_group(*pids);

    // I have no idea if dropping the lock here is correct, and I don't want to think about
    // it. If problems do turn up with another thread observing an intermediate state of
    // this exit operation, the solution is to unify locks. It should be sensible and
    // possible for there to be a single lock that protects all (or nearly all) of the
    // data accessed by both exit and wait. In gvisor and linux this is the lock on the
    // equivalent of the PidTable. This is made more difficult by rust locks being
    // containers that only lock the data they contain, but see
    // https://docs.google.com/document/d/1YHrhBqNhU1WcrsYgGAu3JwwlVmFXPlwWHTJLAbwRebY/edit
    // for an idea.
    std::destroy_at(std::addressof(state));

    // We will need the immediate parent and the reaper. Once we have them, we can make
    // sure to take the locks in the right order: parent before child.
    auto parent = Read()->parent_;
    // auto reaper = state->find_reaper();
    {
      // TODO (Herrera): Reparent the children.
    }

    if (parent.has_value()) {
      auto strong_parent = parent->upgrade();
      ktl::optional<pid_t> tracer_pid;
      {
        // auto task_state = task->Read();
        //  if (task_state->ptrace().has_value()) {
        //    tracer_pid = task_state->ptrace()->get_pid();
        //  }
      }

      ktl::optional<fbl::RefPtr<ZombieProcess>> maybe_zombie = ktl::move(zombie);
      if (tracer_pid.has_value()) {
        /*auto tracer = pids->get_task(tracer_pid.value()).Lock();
        if (tracer) {
          maybe_zombie = tracer->thread_group()->maybe_notify_tracer(
              task, pids, strong_parent.get(), ktl::move(maybe_zombie.value()));
        }*/
      }
      if (maybe_zombie.has_value()) {
        strong_parent->do_zombie_notifications(ktl::move(maybe_zombie.value()));
      }
    } else {
      zombie->release(*pids);
    }

    // TODO: Set the error_code on the Zircon process object. Currently missing a way
    // to do this in Zircon. Might be easier in the new execution model.

    // Once the last zircon thread stops, the zircon process will also stop executing.

    if (parent.has_value()) {
      auto strong_parent = parent->upgrade();
      strong_parent->check_orphans();
    }
  }
}

void ThreadGroup::do_zombie_notifications(fbl::RefPtr<ZombieProcess> zombie) const {
  LTRACEF("zombie pid %d\n", zombie->pid);
  auto state = Write();

  state->children_.erase(zombie->pid);
  /*state->deferred_zombie_ptracers_.erase(
      std::remove_if(state->deferred_zombie_ptracers_.begin(),
                     state->deferred_zombie_ptracers_.end(),
                     [&](const auto& pair) { return pair.second == zombie->pid(); }),
      state->deferred_zombie_ptracers_.end());
*/

  auto exit_signal = zombie->exit_info.exit_signal;
  auto signal_info = zombie->to_wait_result().as_signal_info();

  fbl::AllocChecker ac;
  state->zombie_children_.push_back(ktl::move(zombie), &ac);
  ZX_ASSERT(ac.check());
  state->child_status_waiters_.notify_all();

  // Send signals
  if (exit_signal.has_value()) {
    signal_info.signal = exit_signal.value();
    state->send_signal(signal_info);
  }
}

fit::result<Errno> ThreadGroup::setsid() const {
  {
    auto pids = kernel_->pids_.Write();
    if (pids->get_process_group(leader_).has_value()) {
      return fit::error(errno(EPERM));
    }
    auto process_group = ProcessGroup::New(leader_, ktl::nullopt);
    pids->add_process_group(process_group);
    Write()->set_process_group(process_group, *pids);
  }
  // check_orphans();
  return fit::ok();
}

void ThreadGroup::check_orphans() const {
  fbl::AllocChecker ac;
  fbl::Vector<fbl::RefPtr<ThreadGroup>> thread_groups;

  auto state = Read();
  for (const auto& child : state->children()) {
    thread_groups.push_back(child, &ac);
    ZX_ASSERT(ac.check());
  }

  auto thiz = weak_thread_group_.Lock();
  thread_groups.push_back(ktl::move(thiz), &ac);
  ZX_ASSERT(ac.check());

  // Get unique process groups
  fbl::TaggedHashTable<pid_t, fbl::RefPtr<ProcessGroup>, internal::ThreadGroupTag> process_groups;
  for (const auto& tg : thread_groups) {
    process_groups.insert(tg->Read()->process_group_);
  }

  // Check each process group
  for (auto& process_group : process_groups) {
    process_group.check_orphaned();
  }
}

void ThreadGroup::release() {
  LTRACE_ENTRY_OBJ;
  auto pids = kernel_->pids_.Write();
  auto state = Write();

  for (auto& zombie : state->zombie_children_) {
    zombie->release(*pids);
  }
  state->zombie_children_.reset();

  // state->zombie_ptracees().release(*pids);
  LTRACE_EXIT_OBJ;
}

bool ProcessSelector::do_match(pid_t pid, const PidTable& pid_table) const {
  return ktl::visit(ProcessSelector::overloaded{
                        [](Any) { return true; }, [pid](Pid p) { return p.value == pid; },
                        [pid, &pid_table](Pgid pg) {
                          auto task_ref = pid_table.get_task(pid).Lock();
                          if (task_ref) {
                            if (auto group = pid_table.get_process_group(pg.value)) {
                              if (group.has_value()) {
                                return group == task_ref->thread_group_->Read()->process_group_;
                              }
                            }
                          }
                          return false;
                        }},
                    selector_);
}

void ThreadGroup::ProcessSignalObserver::OnMatch(zx_signals_t signals) {
  canary_.Assert();
  LTRACEF("process signal: 0x%x\n", signals);
}

void ThreadGroup::ProcessSignalObserver::OnCancel(zx_signals_t signals) {
  canary_.Assert();
  LTRACEF("process signal: 0x%x\n", signals);
}

}  // namespace starnix
