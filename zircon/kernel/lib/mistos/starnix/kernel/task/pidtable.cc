// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/task/pidtable.h"

#include <lib/mistos/starnix/kernel/task/process_group.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>
#include <zircon/compiler.h>
#include <zircon/types.h>

#include <optional>
#include <variant>

#include <fbl/ref_ptr.h>
#include <ktl/optional.h>

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

ktl::optional<const PidEntry*> PidTable::get_entry(pid_t pid) {
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
  if (add_overflow(last_pid, 1, &p)) {
    // NB: If/when we re-use pids, we need to check that PidFdFileObject is holding onto
    // the task correctly.
    // track_stub !(TODO("https://fxbug.dev/322874557"), "pid wraparound");
    // last_pid_ = self.last_pid.overflowing_add(1) .0;
  } else {
    last_pid = p;
  }
  return last_pid;
}

util::WeakPtr<Task> PidTable::get_task(pid_t pid) {
  auto entry = get_entry(pid);
  if (entry.has_value()) {
    return entry.value()->task_.value_or(util::WeakPtr<Task>());
  }
  return util::WeakPtr<Task>();
}

void PidTable::add_task(fbl::RefPtr<Task> task) {
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

void PidTable::add_thread_group(fbl::RefPtr<ThreadGroup> thread_group) {
  auto& entry = get_entry_mut(thread_group->leader);
  ASSERT(entry.process_.index() == 0);
  entry.process_ = thread_group;
}

void PidTable::add_process_group(fbl::RefPtr<ProcessGroup> process_group) {
  auto& entry = get_entry_mut(process_group->leader);
  ASSERT(!entry.process_group_.has_value());
  entry.process_group_ = process_group;
}

}  // namespace starnix
