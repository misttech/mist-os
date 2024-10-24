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

void TaskMutableState::update_flags(TaskFlags clear, TaskFlags set) {
  // We don't actually use the guard but we require it to enforce that the
  // caller holds the task's mutable state lock (identified by mutable
  // access to the task's mutable state).

  DEBUG_ASSERT((clear ^ set) == (clear | set));
  TaskFlags observed = base_->flags();
  TaskFlags swapped = base_->flags_.swap((observed | set) & ~clear, std::memory_order_relaxed);
  DEBUG_ASSERT(swapped == observed);
}

bool TaskMutableState::is_any_signal_pending() const {
  starnix_uapi::SigSet mask = this->signal_mask();
  return signals_.is_any_pending() ||
         base_->thread_group()->pending_signals_.Lock()->is_any_allowed_by_mask(mask);
}

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
    task->thread().Write()->value()->SetTask(nullptr);
  }
}
void Task::ThreadSignalObserver::OnCancel(zx_signals_t signals) {
  canary_.Assert();
  LTRACEF("thread signal: 0x%x\n", signals);
}

}  // namespace starnix
