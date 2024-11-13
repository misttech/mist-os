// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_PIDTABLE_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_PIDTABLE_H_

#include <lib/mistos/linux_uapi/typedefs.h>
#include <lib/mistos/memory/weak_ptr.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/mistos/starnix_uapi/signals.h>

#include <fbl/intrusive_hash_table.h>
#include <fbl/intrusive_single_list.h>
#include <fbl/ref_ptr.h>
#include <fbl/vector.h>
#include <ktl/move.h>
#include <ktl/optional.h>
#include <ktl/unique_ptr.h>
#include <ktl/variant.h>

namespace starnix {

using starnix_uapi::Errno;

class ProcessGroup;
class ThreadGroup;
class Task;
class ZombieProcess;

class ProcessEntry {
 public:
  using Variant =
      ktl::variant<ktl::monostate, mtl::WeakPtr<ThreadGroup>, mtl::WeakPtr<ZombieProcess>>;

  static ProcessEntry None();
  static ProcessEntry ThreadGroupCtor(mtl::WeakPtr<ThreadGroup> thread_group);
  static ProcessEntry ZombieProcessCtor(mtl::WeakPtr<ZombieProcess> zombie_process);

  // impl ProcessEntry
  bool is_none() const;

  ktl::optional<std::reference_wrapper<const mtl::WeakPtr<ThreadGroup>>> thread_group() const;

  // C++
  ~ProcessEntry();

 private:
  friend class PidTable;

  // Helpers from the reference documentation for std::visit<>, to allow
  // visit-by-overload of the std::variant<> returned by GetLastReference():
  template <class... Ts>
  struct overloaded : Ts... {
    using Ts::operator()...;
  };
  // explicit deduction guide (not needed as of C++20)
  template <class... Ts>
  overloaded(Ts...) -> overloaded<Ts...>;

  explicit ProcessEntry(Variant variant);

  Variant variant_;
};

class PidEntry : public fbl::SinglyLinkedListable<ktl::unique_ptr<PidEntry>> {
 private:
  ktl::optional<mtl::WeakPtr<Task>> task_;

  ProcessEntry process_ = ProcessEntry::None();

  ktl::optional<mtl::WeakPtr<ProcessGroup>> process_group_;

 public:
  using HashTable = fbl::HashTable<pid_t, ktl::unique_ptr<PidEntry>>;

  // Trait implementation for fbl::HashTable
  pid_t GetKey() const;

  static size_t GetHash(pid_t pid);

  explicit PidEntry(pid_t pid);

  ~PidEntry();

 private:
  friend class PidTable;

  pid_t pid_ = 0;
};

class CurrentTask;
class ProcessEntryRef {
 public:
  using Variant = ktl::variant<fbl::RefPtr<ThreadGroup>, fbl::RefPtr<ZombieProcess>>;

  static ProcessEntryRef Process(fbl::RefPtr<ThreadGroup> thread_group);
  static ProcessEntryRef Zombie(fbl::RefPtr<ZombieProcess> zombie);

  // C++
  ~ProcessEntryRef();

 private:
  // Helpers from the reference documentation for std::visit<>, to allow
  // visit-by-overload of the std::variant<> returned by GetLastReference():
  template <class... Ts>
  struct overloaded : Ts... {
    using Ts::operator()...;
  };
  // explicit deduction guide (not needed as of C++20)
  template <class... Ts>
  overloaded(Ts...) -> overloaded<Ts...>;

  friend class PidTable;
  friend fit::result<Errno> sys_kill(const CurrentTask& current_task, pid_t pid,
                                     starnix_uapi::UncheckedSignal unchecked_signal);

  explicit ProcessEntryRef(Variant variant);

  Variant variant_;
};

class ThreadGroup;
class PidTable {
 private:
  /// The most-recently allocated pid in this table.
  pid_t last_pid_ = 0;

  /// The tasks in this table, organized by pid_t.
  PidEntry::HashTable table_;

  /// Used to notify thread group changes.
  // thread_group_notifier: Option<memory_attribution::sync::Notifier>,

  /// impl PidTable
  ktl::optional<std::reference_wrapper<const PidEntry>> get_entry(pid_t pid) const;

  PidEntry& get_entry_mut(pid_t pid);

  template <typename F>
  void remove_item(pid_t pid, F&& do_remove);

 public:
  pid_t allocate_pid();

  pid_t last_pid() const;

  size_t len() const;

  mtl::WeakPtr<Task> get_task(pid_t pid) const;

  void add_task(const fbl::RefPtr<Task>& task);

  void remove_task(pid_t pid);

  ktl::optional<ProcessEntryRef> get_process(pid_t pid) const;

  fbl::Vector<fbl::RefPtr<ThreadGroup>> get_thread_groups() const;

  void add_thread_group(const fbl::RefPtr<ThreadGroup>& thread_group);

  /// Replace process with the specified `pid` with the `zombie`.
  void kill_process(pid_t pid, mtl::WeakPtr<ZombieProcess> zombie);

  void remove_zombie(pid_t pid);

  void add_process_group(const fbl::RefPtr<ProcessGroup>& process_group);

  ktl::optional<fbl::RefPtr<ProcessGroup>> get_process_group(pid_t pid) const;

  void remove_process_group(pid_t pid);
};

}  // namespace starnix

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_PIDTABLE_H_
