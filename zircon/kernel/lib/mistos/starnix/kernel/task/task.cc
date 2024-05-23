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
#include <lib/mistos/util/weak_wrapper.h>
#include <zircon/compiler.h>
#include <zircon/types.h>

#include <utility>

#include <fbl/ref_ptr.h>
#include <ktl/unique_ptr.h>

namespace starnix {

TaskPersistentInfo TaskPersistentInfoState::New(
    pid_t tid, pid_t pid, fbl::String command,
    Credentials creds /*, exit_signal: Option<Signal>*/) {
  fbl::AllocChecker ac;
  auto info = fbl::AdoptRef(new (&ac) StarnixMutex<TaskPersistentInfoState>(
      TaskPersistentInfoState(tid, pid, command, creds)));
  ASSERT(ac.check());
  return info;
}

fbl::RefPtr<Task> Task::New(pid_t id, const fbl::String& command,
                            fbl::RefPtr<ThreadGroup> thread_group, std::optional<zx::thread> thread,
                            FdTable files, fbl::RefPtr<MemoryManager> mm, fbl::RefPtr<FsContext> fs,
                            Credentials creds) {
  fbl::AllocChecker ac;
  fbl::RefPtr<Task> task =
      fbl::AdoptRef(new (&ac) Task(id, thread_group, std::move(thread), files, mm, fs));
  ASSERT(ac.check());

  pid_t pid = thread_group->leader;
  task->persistent_info = TaskPersistentInfoState::New(id, pid, command, creds);

  return ktl::move(task);
}  // namespace starnix

Task::Task(pid_t _id, fbl::RefPtr<ThreadGroup> _thread_group, ktl::optional<zx::thread> _thread,
           FdTable _files, ktl::optional<fbl::RefPtr<MemoryManager>> mm,
           ktl::optional<fbl::RefPtr<FsContext>> fs)
    : id(_id),
      thread_group(ktl::move(_thread_group)),
      files(_files),
      mm_(std::move(mm)),
      fs_(std::move(fs)) {
  *thread.Write() = ktl::move(_thread);
}

Task::~Task() = default;

const fbl::RefPtr<FsContext>& Task::fs() const {
  ASSERT_MSG(fs_.has_value(), "fs must be set");
  return fs_.value();
}

const fbl::RefPtr<MemoryManager>& Task::mm() const {
  ASSERT_MSG(mm_.has_value(), "mm must be set");
  return mm_.value();
}

const fbl::RefPtr<Kernel>& Task::kernel() const { return thread_group->kernel; }

util::WeakPtr<Task> Task::get_task(pid_t pid) { return kernel()->pids.Read()->get_task(pid); }

pid_t Task::get_pid() const { return thread_group->leader; }

pid_t Task::get_tid() const { return id; }

}  // namespace starnix
