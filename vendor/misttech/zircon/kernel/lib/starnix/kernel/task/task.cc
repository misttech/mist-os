// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/task/task.h"

#include <lib/mistos/starnix/kernel/mm/memory_manager.h>
#include <lib/mistos/starnix/kernel/task/kernel.h>
#include <lib/mistos/starnix/kernel/task/process_group.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>
#include <lib/mistos/starnix/kernel/vfs/dir_entry.h>
#include <lib/mistos/starnix/kernel/vfs/file_object.h>
#include <lib/mistos/starnix/kernel/vfs/fs_context.h>
#include <lib/mistos/starnix/kernel/vfs/fs_node.h>
#include <lib/mistos/starnix_uapi/user_address.h>
#include <lib/mistos/util/default_construct.h>
#include <lib/mistos/util/weak_wrapper.h>
#include <lib/starnix_zircon/task_wrapper.h>
#include <trace.h>
#include <zircon/compiler.h>
#include <zircon/types.h>

#include <utility>

#include <fbl/ref_ptr.h>
#include <ktl/unique_ptr.h>
#include <object/thread_dispatcher.h>

#include "../kernel_priv.h"

#define LOCAL_TRACE STARNIX_KERNEL_GLOBAL_TRACE(0)

namespace starnix {

TaskPersistentInfo TaskPersistentInfoState::New(pid_t tid, pid_t pid,
                                                const ktl::string_view& command,
                                                const Credentials& creds,
                                                ktl::optional<Signal> exit_signal) {
  fbl::AllocChecker ac;
  auto info = fbl::AdoptRef(new (&ac) StarnixMutex<TaskPersistentInfoState>(
      TaskPersistentInfoState(tid, pid, command, creds, exit_signal)));
  ASSERT(ac.check());
  return info;
}

void TaskMutableState::copy_state_from(const CurrentTask& current_task) {
  captured_thread_state_ = ktl::optional<CapturedThreadState>(CapturedThreadState{
      .thread_state = current_task.thread_state().extended_snapshot(), .dirty = false});
}

void TaskMutableState::store_stopped(StopState state) {
  // We don't actually use the guard but we require it to enforce that the
  // caller holds the thread group's mutable state lock (identified by
  // mutable access to the thread group's mutable state).

  base_->stop_state_.store(state, std::memory_order_relaxed);
}

bool TaskMutableState::is_blocked() const {
  LTRACE_ENTRY_OBJ;
  LTRACEF("task %p\n", base_);
  return signals_.run_state_.is_blocked();
}

void TaskMutableState::set_run_state(RunState run_state) {
  LTRACE_ENTRY_OBJ;
  LTRACEF("task %p\n", base_);
  signals_.run_state_ = ktl::move(run_state);
}

void TaskMutableState::set_stopped(
    StopState stopped, ktl::optional<SignalInfo> siginfo,
    ktl::optional<std::reference_wrapper<const CurrentTask>> current_task,
    ktl::optional<PtraceEventData> event) {
  if (StopStateHelper::ptrace_only(stopped) && !ptrace_) {
    return;
  }

  if (StopStateHelper::is_illegal_transition(base_->load_stopped(), stopped)) {
    return;
  }

  // TODO(https://fxbug.dev/306438676): When task can be
  // stopped inside user code, task will need to be either restarted or
  // stopped here.
  store_stopped(stopped);
  if (StopStateHelper::is_stopped(stopped)) {
    if (current_task.has_value()) {
      copy_state_from(current_task.value());
    }
  }

  if (ptrace_) {
    // ptrace_->set_last_signal(siginfo);
    // ptrace_->set_last_event(event);
  }

  if (stopped == StopState::Waking || stopped == StopState::ForceWaking) {
    // notify_ptracees();
  }
  if (!StopStateHelper::is_in_progress(stopped)) {
    // notify_ptracers();
  }
}

void TaskMutableState::update_flags(TaskFlags clear, TaskFlags set) {
  // We don't actually use the guard but we require it to enforce that the
  // caller holds the task's mutable state lock (identified by mutable
  // access to the task's mutable state).

  DEBUG_ASSERT((clear ^ set) == (clear | set));
  TaskFlags observed = base_->flags();
  TaskFlags swapped = base_->flags_.swap((observed | set) & ~clear, std::memory_order_relaxed);
  DEBUG_ASSERT(swapped == observed);
  LTRACEF_LEVEL(2, "pid:%d flags:0x%x\n", base_->id_, base_->flags().bits());
}

/// Returns the number of pending signals for this task, without considering the signal mask.
size_t TaskMutableState::pending_signal_count() const {
  return signals_.num_queued() + base_->thread_group()->pending_signals_.Lock()->num_queued();
}

bool TaskMutableState::has_signal_pending(Signal signal) const {
  return signals_.has_queued(signal) ||
         base_->thread_group()->pending_signals_.Lock()->has_queued(signal);
}

SigSet TaskMutableState::pending_signals() const {
  return signals_.pending() | base_->thread_group()->pending_signals_.Lock()->pending();
}

SigSet TaskMutableState::task_specific_pending_signals() const { return signals_.pending(); }

bool TaskMutableState::is_any_signal_allowed_by_mask(SigSet mask) const {
  return signals_.is_any_allowed_by_mask(mask) ||
         base_->thread_group()->pending_signals_.Lock()->is_any_allowed_by_mask(mask);
}

bool TaskMutableState::is_any_signal_pending() const {
  starnix_uapi::SigSet mask = this->signal_mask();
  return signals_.is_any_pending() ||
         base_->thread_group()->pending_signals_.Lock()->is_any_allowed_by_mask(mask);
}

template <typename F>
ktl::optional<SignalInfo> TaskMutableState::take_next_signal_where(F&& predicate) {
  auto thread_group_signal =
      base_->thread_group()->pending_signals_.Lock()->take_next_where(predicate);
  if (thread_group_signal.has_value()) {
    return thread_group_signal;
  }
  return signals_.take_next_where(predicate);
}

/// Removes and returns the next pending `signal` for this task.
///
/// Returns `None` if `siginfo` is a blocked signal, or no such signal is pending.
ktl::optional<SignalInfo> TaskMutableState::take_specific_signal(const SignalInfo& siginfo) {
  SigSet signal_mask = signals_.mask();
  if (signal_mask.has_signal(siginfo.signal)) {
    return ktl::nullopt;
  }

  auto predicate = [&siginfo](const SignalInfo& s) { return s.signal == siginfo.signal; };
  return take_next_signal_where(predicate);
}

/// Removes and returns a pending signal that is unblocked by the current signal mask.
///
/// Returns `None` if there are no unblocked signals pending.
ktl::optional<SignalInfo> TaskMutableState::take_any_signal() {
  return take_signal_with_mask(signals_.mask());
}

/// Removes and returns a pending signal that is unblocked by `signal_mask`.
///
/// Returns `None` if there are no signals pending that are unblocked by `signal_mask`.
ktl::optional<SignalInfo> TaskMutableState::take_signal_with_mask(SigSet signal_mask) {
  auto predicate = [&signal_mask](const SignalInfo& s) {
    return !signal_mask.has_signal(s.signal) || s.force;
  };
  return take_next_signal_where(predicate);
}

#ifdef CONFIG_STARNIX_TEST
size_t TaskMutableState::queued_signal_count(Signal signal) const {
  return signals_.queued_count(signal) +
         thread_group_->pending_signals().lock().queued_count(signal);
}
#endif

fbl::RefPtr<Task> Task::New(pid_t id, const ktl::string_view& command,
                            fbl::RefPtr<ThreadGroup> thread_group,
                            ktl::optional<fbl::RefPtr<ThreadDispatcher>> thread, FdTable files,
                            fbl::RefPtr<MemoryManager> mm, fbl::RefPtr<FsContext> fs,
                            Credentials creds, ktl::optional<Signal> exit_signal,
                            SigSet signal_mask, bool no_new_privs, uint64_t timerslack_ns) {
  fbl::AllocChecker ac;
  fbl::RefPtr<Task> task =
      fbl::AdoptRef(new (&ac) Task(id, thread_group, ktl::move(thread), ktl::move(files), mm, fs,
                                   exit_signal, signal_mask, no_new_privs, timerslack_ns));
  ASSERT(ac.check());

  pid_t pid = thread_group->leader();
  task->persistent_info_ = TaskPersistentInfoState::New(id, pid, command, creds, exit_signal);

  return ktl::move(task);
}  // namespace starnix

fit::result<Errno, FdNumber> Task::add_file(FileHandle file, FdFlags flags) const {
  return files_.add_with_flags(*this, file, flags);
}

Task::Task(pid_t id, fbl::RefPtr<ThreadGroup> thread_group,
           ktl::optional<fbl::RefPtr<ThreadDispatcher>> thread, FdTable files,
           ktl::optional<fbl::RefPtr<MemoryManager>> mm, ktl::optional<fbl::RefPtr<FsContext>> fs,
           ktl::optional<Signal> exit_signal, SigSet signal_mask, bool no_new_privs,
           uint64_t timerslack_ns)
    : id_(id),
      thread_group_(ktl::move(thread_group)),
      files_(files),
      mm_(ktl::move(mm)),
      fs_(ktl::move(fs)),
      stop_state_(AtomicStopState(StopState::Awake)),
      flags_(AtomicTaskFlags(TaskFlags::empty())),
      mutable_state_(ktl::move(TaskMutableState(
          this, mtl::DefaultConstruct<UserRef<pid_t>>(), SignalState::with_mask(signal_mask), {},
          no_new_privs, timerslack_ns,
          /*The default timerslack is set to the current timerslack of the creating thread.*/
          timerslack_ns))),
      observer_(util::WeakPtr(this)) {
  *thread_.Write() = ktl::move(thread);

  LTRACE_ENTRY_OBJ;
}

Task::~Task() { LTRACE_ENTRY_OBJ; }

void Task::release(ThreadState context) {
  LTRACE_ENTRY_OBJ;

  // ZX_ASSERT(IsLastReference());

  // Release the fd table
  files_.release();

  // Signal vfork completion if needed
  // signal_vfork();

  // Drop fields that can end up owning a FsNode to ensure no FsNode are owned by this task
  fs_.reset();
  mm_.reset();

  // Rebuild a temporary CurrentTask to run the release actions that require a CurrentState
  auto current_task = CurrentTask::New(fbl::RefPtr<Task>(this), context);

  // Apply any delayed releasers left
  // current_task.trigger_delayed_releaser();

  // auto task = current_task.task().reset();
  //  Release the ThreadGroup
  thread_group_->release();

  LTRACE_EXIT_OBJ;
}

fbl::RefPtr<FsContext> Task::fs() const {
  ASSERT_MSG(fs_.has_value(), "fs must be set");
  return *fs_->Read();
}

const fbl::RefPtr<MemoryManager>& Task::mm() const {
  ASSERT_MSG(mm_.has_value(), "mm must be set");
  return mm_.value();
}

fbl::RefPtr<Kernel>& Task::kernel() const { return thread_group_->kernel(); }

util::WeakPtr<Task> Task::get_task(pid_t pid) const { return kernel()->pids.Read()->get_task(pid); }

pid_t Task::get_pid() const { return thread_group_->leader(); }

void Task::interrupt() const {
  LTRACE_ENTRY_OBJ;
  Read()->signals_.run_state_.wake();

  if (auto ts = thread_.Read(); ts->has_value()) {
    // TODO (Herrera)
    // crate::execution::interrupt_thread(thread);
  }
}

struct sigaction Task::get_signal_action(Signal signal) const {
  return thread_group_->signal_actions()->Get(signal);
}

fit::result<Errno, ktl::span<uint8_t>> Task::read_memory(UserAddress addr,
                                                         ktl::span<uint8_t>& bytes) const {
  return (*mm_)->syscall_read_memory(addr, bytes);
}

fit::result<Errno, ktl::span<uint8_t>> Task::read_memory_partial_until_null_byte(
    UserAddress addr, ktl::span<uint8_t>& bytes) const {
  return (*mm_)->syscall_read_memory_partial_until_null_byte(addr, bytes);
}

fit::result<Errno, ktl::span<uint8_t>> Task::read_memory_partial(UserAddress addr,
                                                                 ktl::span<uint8_t>& bytes) const {
  return (*mm_)->syscall_read_memory_partial(addr, bytes);
}

fit::result<Errno, size_t> Task::write_memory(UserAddress addr,
                                              const ktl::span<const uint8_t>& bytes) const {
  return (*mm_)->syscall_write_memory(addr, bytes);
}

fit::result<Errno, size_t> Task::write_memory_partial(UserAddress addr,
                                                      const ktl::span<const uint8_t>& bytes) const {
  return (*mm_)->syscall_write_memory_partial(addr, bytes);
}

fit::result<Errno, size_t> Task::zero(UserAddress addr, size_t length) const {
  return (*mm_)->syscall_zero(addr, length);
}

void Task::ThreadSignalObserver::OnMatch(zx_signals_t signals) {
  canary_.Assert();
  LTRACEF("thread signal: 0x%x\n", signals);

  auto task = task_.Lock();
  if (task) {
    // Set the current TaskWrapper to null to force CurrentTask to release.
    task->thread_.Write()->value()->SetTask(nullptr);
  }
}
void Task::ThreadSignalObserver::OnCancel(zx_signals_t signals) {
  canary_.Assert();
  LTRACEF("thread signal: 0x%x\n", signals);
}

}  // namespace starnix
