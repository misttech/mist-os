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
#include <zircon/compiler.h>
#include <zircon/types.h>

#include <cassert>
#include <utility>

#include <fbl/ref_ptr.h>
#include <ktl/unique_ptr.h>

namespace starnix {

bool TaskMutableState::Initialize() { return true; }

zx_status_t Task::New(pid_t id, const fbl::String& command, fbl::RefPtr<ThreadGroup> thread_group,
                      std::optional<zx::thread> thread, FdTable files,
                      fbl::RefPtr<MemoryManager> mm, fbl::RefPtr<FsContext> fs,
                      fbl::RefPtr<Task>* out) {
  fbl::AllocChecker ac;
  pid_t pid = thread_group->leader();
  fbl::RefPtr<Task> task =
      fbl::AdoptRef(new (&ac) Task(id, thread_group, std::move(thread), files, mm, fs,
                                   TaskPersistentInfoState{id, pid, command}));
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  *out = std::move(task);
  return ZX_OK;
}  // namespace starnix

Task::Task(pid_t id, fbl::RefPtr<ThreadGroup> thread_group, std::optional<zx::thread> thread,
           FdTable files, ktl::optional<fbl::RefPtr<MemoryManager>> mm,
           ktl::optional<fbl::RefPtr<FsContext>> fs, TaskPersistentInfoState persistent_info)
    : id_(id),
      thread_group_(std::move(thread_group)),
      thread_(std::move(thread)),
      files_(files),
      mm_(std::move(mm)),
      fs_(std::move(fs)) {
  mutable_state_.Initialize();

  fbl::AllocChecker ac;
  persistent_info_ = fbl::MakeRefCountedChecked<TaskPersistentInfoLock>(&ac, persistent_info);
  ASSERT(ac.check());
}

Task::~Task() {}

const fbl::RefPtr<ThreadGroup>& Task::thread_group() const { return thread_group_; }

const fbl::RefPtr<FsContext>& Task::fs() const {
  ASSERT_MSG(fs_.has_value(), "fs must be set");
  return fs_.value();
}

const fbl::RefPtr<MemoryManager>& Task::mm() const {
  ASSERT_MSG(mm_.has_value(), "mm must be set");
  return mm_.value();
}

const fbl::RefPtr<Kernel>& Task::kernel() const { return thread_group_->kernel(); }

fbl::RefPtr<Task> Task::get_task(pid_t pid) {
  Guard<Mutex> lock(kernel()->pidtable_rw_lock());
  return kernel()->pids().get_task(pid);
}

pid_t Task::get_pid() const { return thread_group_->leader(); }

pid_t Task::get_tid() const { return id_; }

}  // namespace starnix
