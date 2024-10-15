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
#include <object/thread_dispatcher.h>

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

fbl::RefPtr<Task> Task::New(pid_t id, const ktl::string_view& command,
                            fbl::RefPtr<ThreadGroup> thread_group,
                            ktl::optional<fbl::RefPtr<ThreadDispatcher>> thread, FdTable files,
                            fbl::RefPtr<MemoryManager> mm, fbl::RefPtr<FsContext> fs,
                            Credentials creds, ktl::optional<Signal> exit_signal) {
  fbl::AllocChecker ac;
  fbl::RefPtr<Task> task =
      fbl::AdoptRef(new (&ac) Task(id, thread_group, ktl::move(thread), ktl::move(files), mm, fs));
  ASSERT(ac.check());

  pid_t pid = thread_group->leader();
  task->persistent_info = TaskPersistentInfoState::New(id, pid, command, creds, exit_signal);

  return ktl::move(task);
}  // namespace starnix

fit::result<Errno, FdNumber> Task::add_file(FileHandle file, FdFlags flags) const {
  return files_.add_with_flags(*this, file, flags);
}

Task::Task(pid_t id, fbl::RefPtr<ThreadGroup> thread_group,
           ktl::optional<fbl::RefPtr<ThreadDispatcher>> thread, FdTable files,
           ktl::optional<fbl::RefPtr<MemoryManager>> mm, ktl::optional<fbl::RefPtr<FsContext>> fs)
    : id_(id),
      thread_group_(ktl::move(thread_group)),
      files_(files),
      mm_(ktl::move(mm)),
      fs_(ktl::move(fs)) {
  //*thread.Write() = ktl::move(_thread);
}

Task::~Task() = default;

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

}  // namespace starnix
