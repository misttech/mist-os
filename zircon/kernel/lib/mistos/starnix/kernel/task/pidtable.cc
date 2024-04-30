// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/task/pidtable.h"

#include <lib/mistos/starnix/kernel/task/process_group.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>
#include <zircon/compiler.h>
#include <zircon/types.h>

#include <cassert>
#include <optional>
#include <variant>

#include "fbl/ref_ptr.h"

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

fbl::RefPtr<Task> PidTable::get_task(pid_t pid) {
  const auto& entry = table_.find(pid);
  if (entry == table_.end()) {
    return fbl::RefPtr<Task>();
  }
  return entry->task().value_or(fbl::RefPtr<Task>());
}

void PidTable::add_task(fbl::RefPtr<Task> task) {
  fbl::AllocChecker ac;
  auto ptr = ktl::make_unique<PidEntry>(&ac, task->get_tid(), std::optional<fbl::RefPtr<Task>>(),
                                        ProcessEntry(), std::optional<fbl::RefPtr<ProcessGroup>>());
  if (ac.check()) {
    table_.insert_or_find(std::move(ptr));

    const auto& entry = table_.find(task->get_tid());
    ASSERT(!entry->task().has_value());
    entry->task() = task;
  }
}

void PidTable::remove_task(pid_t pid) {
  const auto& entry = table_.find(pid);
  if (entry != table_.end()) {
    // if (!entry->process_group.has_value() && !entry->task.has_value()) {
    //   table_.erase(pid);
    // }
  }
}

void PidTable::add_thread_group(fbl::RefPtr<ThreadGroup> thread_group) {
  fbl::AllocChecker ac;
  auto ptr =
      ktl::make_unique<PidEntry>(&ac, thread_group->leader(), std::optional<fbl::RefPtr<Task>>(),
                                 ProcessEntry(), std::optional<fbl::RefPtr<ProcessGroup>>());
  if (ac.check()) {
    table_.insert_or_find(std::move(ptr));
    const auto& entry = table_.find(thread_group->leader());
    ASSERT(entry->process().index() == 0);
    entry->process() = thread_group;
  }
}

void PidTable::add_process_group(fbl::RefPtr<ProcessGroup> process_group) {
  fbl::AllocChecker ac;
  auto ptr =
      ktl::make_unique<PidEntry>(&ac, process_group->leader(), std::optional<fbl::RefPtr<Task>>(),
                                 ProcessEntry(), std::optional<fbl::RefPtr<ProcessGroup>>());
  if (ac.check()) {
    table_.insert_or_find(std::move(ptr));

    const auto& entry = table_.find(process_group->leader());
    ASSERT(!entry->process_group().has_value());
    entry->process_group() = process_group;
  }
}

}  // namespace starnix
