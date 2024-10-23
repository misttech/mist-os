// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/task/pid_table.h"

#include <lib/mistos/starnix/kernel/task/process_group.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>
#include <lib/mistos/util/num.h>
#include <lib/mistos/util/weak_wrapper.h>
#include <zircon/assert.h>
#include <zircon/compiler.h>
#include <zircon/types.h>

#include <optional>
#include <variant>

#include <fbl/ref_ptr.h>
#include <ktl/optional.h>

// #include <ktl/enforce.h>

namespace starnix {

ProcessEntry ProcessEntry::None() { return ProcessEntry({}); }

ProcessEntry ProcessEntry::ThreadGroupCtor(util::WeakPtr<ThreadGroup> thread_group) {
  return ProcessEntry(thread_group);
}
ProcessEntry ProcessEntry::ZombieProcessCtor(util::WeakPtr<ZombieProcess> zombie_process) {
  return ProcessEntry(zombie_process);
}

ProcessEntry::~ProcessEntry() = default;
ProcessEntry::ProcessEntry(Variant variant) : process_(ktl::move(variant)) {}

bool ProcessEntry::is_none() const { return ktl::holds_alternative<ktl::monostate>(process_); }

ktl::optional<std::reference_wrapper<const util::WeakPtr<ThreadGroup>>> ProcessEntry::thread_group()
    const {
  if (auto* ptr = ktl::get_if<util::WeakPtr<ThreadGroup>>(&process_)) {
    return std::reference_wrapper<const util::WeakPtr<ThreadGroup>>(*ptr);
  }
  return ktl::nullopt;
}

ktl::optional<std::reference_wrapper<const util::WeakPtr<ZombieProcess>>> ProcessEntry::zombie()
    const {
  if (auto* ptr = ktl::get_if<util::WeakPtr<ZombieProcess>>(&process_)) {
    return std::reference_wrapper<const util::WeakPtr<ZombieProcess>>(*ptr);
  }
  return ktl::nullopt;
}

pid_t PidEntry::GetKey() const { return pid_; }

size_t PidEntry::GetHash(pid_t pid) { return pid; }

PidEntry::PidEntry(pid_t pid) : pid_(pid) {}

PidEntry::~PidEntry() = default;

ktl::optional<const PidEntry*> PidTable::get_entry(pid_t pid) const {
  const auto& entry = table_.find(pid);
  if (entry == table_.end()) {
    return ktl::nullopt;
  }
  return &(*entry);
}

PidEntry& PidTable::get_entry_mut(pid_t pid) {
  fbl::AllocChecker ac;
  auto default_ptr = ktl::make_unique<PidEntry>(&ac, pid);
  if (ac.check()) {
    table_.insert_or_find(ktl::move(default_ptr));
  }
  return *table_.find(pid);
}

template <typename F>
void PidTable::remove_item(pid_t pid, F&& do_remove) {
  auto& entry = get_entry_mut(pid);
  do_remove(entry);
  if (!entry.task_.has_value() && entry.process_.is_none() && !entry.process_group_.has_value()) {
    table_.erase(pid);
  }
}

pid_t PidTable::allocate_pid() {
  auto result = mtl::checked_add(last_pid_, 1);
  if (result.has_value()) {
    last_pid_ = result.value();
  } else {
    // NB: If/when we re-use pids, we need to check that PidFdFileObject is holding onto
    // the task correctly.
    // track_stub !(TODO("https://fxbug.dev/322874557"), "pid wraparound");
    last_pid_ = mtl::overflowing_add(last_pid_, 1).first;
  }
  return last_pid_;
}

pid_t PidTable::last_pid() const { return last_pid_; }

size_t PidTable::len() const { return table_.size(); }

util::WeakPtr<Task> PidTable::get_task(pid_t pid) const {
  auto entry = get_entry(pid);
  if (entry.has_value()) {
    return (*entry)->task_.value_or(util::WeakPtr<Task>());
  }
  return util::WeakPtr<Task>();
}

void PidTable::add_task(const fbl::RefPtr<Task>& task) {
  auto& entry = get_entry_mut(task->id());
  ASSERT(!entry.task_.has_value());
  entry.task_ = util::WeakPtr<Task>(task.get());
}

void PidTable::remove_task(pid_t pid) {
  remove_item(pid, [](auto& entry) {
    auto removed = ktl::move(entry.task_);
    entry.task_ = ktl::nullopt;
    ZX_ASSERT(removed.has_value());
  });
}

void PidTable::add_thread_group(const fbl::RefPtr<ThreadGroup>& thread_group) {
  auto& entry = get_entry_mut(thread_group->leader());
  ASSERT(entry.process_.is_none());
  entry.process_ = ProcessEntry::ThreadGroupCtor(util::WeakPtr<ThreadGroup>(thread_group.get()));
}

void PidTable::kill_process(pid_t pid, util::WeakPtr<ZombieProcess> zombie) {
  auto& entry = get_entry_mut(pid);
  ZX_ASSERT(entry.process_.thread_group().has_value());

  // All tasks from the process are expected to be cleared from the table before the process
  // becomes a zombie. We can't verify this for all tasks here, check it just for the leader.
  ZX_ASSERT(!entry.task_.has_value());

  entry.process_ = ProcessEntry::ZombieProcessCtor(zombie);
}

void PidTable::remove_zombie(pid_t pid) {
  remove_item(pid, [](auto& entry) {
    ZX_ASSERT(entry.process_.zombie().has_value());
    entry.process_ = ProcessEntry::None();
  });

  // Notify thread group changes.
  // TODO: Implement thread group change notification mechanism
  // if (thread_group_notifier_) {
  //   thread_group_notifier_->Notify();
  // }
}

void PidTable::add_process_group(const fbl::RefPtr<ProcessGroup>& process_group) {
  auto& entry = get_entry_mut(process_group->leader());
  ASSERT(!entry.process_group_.has_value());
  entry.process_group_ = util::WeakPtr<ProcessGroup>(process_group.get());
}

ktl::optional<fbl::RefPtr<ProcessGroup>> PidTable::get_process_group(pid_t pid) const {
  auto entry = get_entry(pid);
  if (entry.has_value() && (*entry)->process_group_) {
    return (*entry)->process_group_->Lock();
  }
  return ktl::nullopt;
}

void PidTable::remove_process_group(pid_t pid) {
  remove_item(pid, [](auto& entry) {
    auto removed = ktl::move(entry.process_group_);
    ZX_ASSERT(removed.has_value());
  });
}

}  // namespace starnix
