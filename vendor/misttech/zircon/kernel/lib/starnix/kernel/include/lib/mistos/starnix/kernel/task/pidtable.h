// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_PIDTABLE_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_PIDTABLE_H_

#include <lib/mistos/linux_uapi/typedefs.h>
#include <lib/mistos/util/weak_wrapper.h>

#include <utility>

#include <fbl/intrusive_hash_table.h>
#include <fbl/intrusive_single_list.h>
#include <fbl/ref_ptr.h>
#include <ktl/move.h>
#include <ktl/optional.h>
#include <ktl/unique_ptr.h>
#include <ktl/variant.h>

namespace starnix {

class ProcessGroup;
class ThreadGroup;
class Task;
class ZombieProcess;

using ProcessEntry =
    ktl::variant<std::monostate, util::WeakPtr<ThreadGroup>, util::WeakPtr<ZombieProcess>>;

struct PidEntry : public fbl::SinglyLinkedListable<ktl::unique_ptr<PidEntry>> {
 private:
  ktl::optional<util::WeakPtr<Task>> task_;

  ProcessEntry process_;

  ktl::optional<util::WeakPtr<ProcessGroup>> process_group_;

 public:
  using HashTable = fbl::HashTable<pid_t, ktl::unique_ptr<PidEntry>>;

  // Trait implementation for fbl::HashTable
  pid_t GetKey() const;

  static size_t GetHash(pid_t pid);

  PidEntry(pid_t pid);

  ~PidEntry();

 private:
  friend class PidTable;

  pid_t pid_ = 0;
};

class PidTable {
 public:
  /// The most-recently allocated pid in this table.
  pid_t last_pid_ = 0;

  /// The tasks in this table, organized by pid_t.
  PidEntry::HashTable table;

 private:
  /// impl PidTable
  ktl::optional<const PidEntry*> get_entry(pid_t pid) const;

  PidEntry& get_entry_mut(pid_t pid);

 public:
  pid_t allocate_pid();

  pid_t last_pid() const;

  size_t len() const;

  util::WeakPtr<Task> get_task(pid_t pid) const;

  void add_task(const fbl::RefPtr<Task>& task);

  void remove_task(pid_t pid);

  void add_thread_group(const fbl::RefPtr<ThreadGroup>& thread_group);

  void add_process_group(const fbl::RefPtr<ProcessGroup>& process_group);
};

}  // namespace starnix

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_PIDTABLE_H_
