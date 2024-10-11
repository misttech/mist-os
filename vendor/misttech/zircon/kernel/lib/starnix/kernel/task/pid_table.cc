// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/task/pid_table.h"

#include <lib/mistos/starnix/kernel/task/process_group.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>
#include <lib/mistos/starnix/kernel/task/zombie_process.h>
#include <zircon/compiler.h>
#include <zircon/types.h>

#include <optional>
#include <variant>

#include <fbl/ref_ptr.h>
#include <ktl/optional.h>

#include "lib/mistos/util/weak_wrapper.h"

#include <ktl/enforce.h>

namespace {
#if 0
template <typename K, class V>
V&& get(const fbl::HashTable<K, V>& table, const K& key) {
  auto iter = table.find(key);
  if (iter != table.end() && iter.IsValid()) {
    return *iter;
  }
}
#endif
}  // namespace

namespace starnix {

pid_t PidEntry::GetKey() const { return pid_; }

size_t PidEntry::GetHash(pid_t pid) { return pid; }

PidEntry::PidEntry(pid_t pid) : pid_(pid) {}

PidEntry::~PidEntry() = default;

ktl::optional<const PidEntry*> PidTable::get_entry(pid_t pid) const {
  const auto& entry = table.find(pid);
  if (entry == table.end()) {
    return ktl::nullopt;
  }
  return &(*entry);
}

PidEntry& PidTable::get_entry_mut(pid_t pid) {
  fbl::AllocChecker ac;
  auto default_ptr = ktl::make_unique<PidEntry>(&ac, pid);
  if (ac.check()) {
    table.insert_or_find(ktl::move(default_ptr));
  }
  return *table.find(pid);
}

pid_t PidTable::allocate_pid() {
  pid_t p;
  if (add_overflow(last_pid_, 1, &p)) {
    // NB: If/when we re-use pids, we need to check that PidFdFileObject is holding onto
    // the task correctly.
    // track_stub !(TODO("https://fxbug.dev/322874557"), "pid wraparound");
    // last_pid_ = self.last_pid.overflowing_add(1) .0;
  } else {
    last_pid_ = p;
  }
  return last_pid_;
}

pid_t PidTable::last_pid() const { return last_pid_; }

size_t PidTable::len() const { return table.size(); }

util::WeakPtr<Task> PidTable::get_task(pid_t pid) const {
  auto entry = get_entry(pid);
  if (entry.has_value()) {
    return entry.value()->task_.value_or(util::WeakPtr<Task>());
  }
  return util::WeakPtr<Task>();
}

void PidTable::add_task(const fbl::RefPtr<Task>& task) {
  auto& entry = get_entry_mut(task->id);
  ASSERT(!entry.task_.has_value());
  entry.task_ = util::WeakPtr<Task>(task.get());
}

void PidTable::remove_task(pid_t pid) {
  const auto& entry = table.find(pid);
  if (entry != table.end()) {
    // if (!entry->process_group.has_value() && !entry->task.has_value()) {
    //   table_.erase(pid);
    // }
  }
}

void PidTable::add_thread_group(const fbl::RefPtr<ThreadGroup>& thread_group) {
  auto& entry = get_entry_mut(thread_group->leader);
  ASSERT(entry.process_.index() == 0);
  entry.process_ = util::WeakPtr<ThreadGroup>(thread_group.get());
}

void PidTable::add_process_group(const fbl::RefPtr<ProcessGroup>& process_group) {
  auto& entry = get_entry_mut(process_group->leader);
  ASSERT(!entry.process_group_.has_value());
  entry.process_group_ = util::WeakPtr<ProcessGroup>(process_group.get());
}

}  // namespace starnix
