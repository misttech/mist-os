// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_PIDTABLE_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_PIDTABLE_H_

#include <lib/mistos/linux_uapi/typedefs.h>
#include <lib/mistos/starnix/kernel/task/forward.h>

#include <optional>
#include <utility>
#include <variant>

#include <fbl/intrusive_hash_table.h>
#include <fbl/intrusive_single_list.h>
#include <fbl/ref_ptr.h>
#include <ktl/move.h>
#include <ktl/unique_ptr.h>

namespace starnix {

class ZombieProcess {
  // pid_t pid;
  // pid_t pgid;
  // uid_t pid_tuid;

  // pub exit_info: ProcessExitInfo,

  /// Cumulative time stats for the process and its children.
  // pub time_stats: TaskTimeStats,

  // Whether dropping this ZombieProcess should imply removing the pid from
  // the PidTable
  // bool is_canonical_;
};

using ProcessEntry = std::variant<std::monostate, fbl::RefPtr<ThreadGroup>, ZombieProcess>;

struct PidEntry : public fbl::SinglyLinkedListable<ktl::unique_ptr<PidEntry>> {
 public:
  using HashTable = fbl::HashTable<pid_t, ktl::unique_ptr<PidEntry>>;

  PidEntry(pid_t pid, std::optional<fbl::RefPtr<Task>> task, ProcessEntry process,
           std::optional<fbl::RefPtr<ProcessGroup>> pg)
      : pid_(pid),
        task_(std::move(task)),
        process_(std::move(process)),
        process_group_(std::move(pg)) {}

  // Trait implementation for fbl::HashTable
  pid_t GetKey() const { return pid_; }
  static size_t GetHash(pid_t pid) { return pid; }

  std::optional<fbl::RefPtr<Task>>& task() { return task_; }
  ProcessEntry& process() { return process_; }
  std::optional<fbl::RefPtr<ProcessGroup>>& process_group() { return process_group_; }

 private:
  pid_t pid_ = 0;
  std::optional<fbl::RefPtr<Task>> task_;
  ProcessEntry process_;
  std::optional<fbl::RefPtr<ProcessGroup>> process_group_;
};

class PidTable {
 public:
  pid_t allocate_pid();
  pid_t last_pid() const { return last_pid_; }
  size_t len() const { return table_.size(); }

  fbl::RefPtr<Task> get_task(pid_t pid);
  void add_task(fbl::RefPtr<Task> task);
  void remove_task(pid_t pid);

  void add_thread_group(fbl::RefPtr<ThreadGroup> thread_group);

  void add_process_group(fbl::RefPtr<ProcessGroup> process_group);

 private:
  // The most-recently allocated pid in this table.
  pid_t last_pid_ = 0;

  // The tasks in this table, organized by pid_t.
  PidEntry::HashTable table_;
};

}  // namespace starnix

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_PIDTABLE_H_
