// Copyright 2021 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_FD_TABLE_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_FD_TABLE_H_

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/sync/locks.h>
#include <lib/mistos/starnix/kernel/vfs/fd_numbers.h>
#include <lib/mistos/starnix/kernel/vfs/forward.h>
#include <lib/mistos/util/back_insert_iterator.h>
#include <lib/mistos/util/bitflags.h>
#include <zircon/compiler.h>

#include <atomic>
#include <functional>
#include <utility>

#include <fbl/ref_counted.h>
#include <fbl/vector.h>
#include <ktl/algorithm.h>
#include <ktl/move.h>
#include <ktl/optional.h>

#include <linux/fcntl.h>

namespace starnix {

enum class FdFlagsEnum : uint32_t {
  CLOEXEC = FD_CLOEXEC,
};

using FdFlags = Flags<FdFlagsEnum>;

struct FdTableId {
  size_t id;
};

struct FdTableEntry {
 public:
  FileHandle file;

 private:
  // Identifier of the FdTable containing this entry.
  FdTableId fd_table_id_;

  // Rather than using a separate "flags" field, we could maintain this data
  // as a bitfield over the file descriptors because there is only one flag
  // currently (CLOEXEC) and file descriptor numbers tend to cluster near 0.
  FdFlags flags_;

 public:
  FdTableEntry(FileHandle _file, FdTableId fd_table_id, FdFlags flags)
      : file(ktl::move(_file)), fd_table_id_(fd_table_id), flags_(flags) {}

  ~FdTableEntry();

 private:
  friend class FdTable;
  friend class FdTableInner;
};

/// Having the map a separate data structure allows us to memoize next_fd, which is the
/// lowest numbered file descriptor not in use.
class FdTableStore {
 private:
  fbl::Vector<ktl::optional<FdTableEntry>> entries_;

  FdNumber next_fd_ = FdNumber::from_raw(0);

 public:
  /// impl FdTableStore
  fit::result<Errno, ktl::optional<FdTableEntry>> insert_entry(FdNumber fd, uint64_t rlimit,
                                                               FdTableEntry entry);

  ktl::optional<FdTableEntry> remove_entry(const FdNumber& fd);

  ktl::optional<FdTableEntry> get(FdNumber fd) const;

  ktl::optional<std::reference_wrapper<ktl::optional<FdTableEntry>>> get_mut(FdNumber fd);

  // Recalculates the lowest available FD >= minfd based on the contents of the map.
  FdNumber calculate_lowest_available_fd(FdNumber minfd) const;

  void retain(std::function<bool(const FdNumber&, FdTableEntry&)> func);

 public:
  FdTableStore() = default;

  FdTableStore(const FdTableStore& other) {
    fbl::AllocChecker ac;
    entries_.reserve(other.entries_.capacity(), &ac);
    ASSERT(ac.check());

    ktl::copy(other.entries_.begin(), other.entries_.end(), util::back_inserter(entries_));
    next_fd_ = other.next_fd_;
  }

  FdTableStore& operator=(FdTableStore&& other) = default;

 private:
  friend class FdTableInner;
  friend class FdTable;
};

class FdTableInner : public fbl::RefCounted<FdTableInner> {
 public:
  FdTableId id() const { return {reinterpret_cast<size_t>(&store_.Lock()->entries_)}; }

  fbl::RefPtr<FdTableInner> unshare() {
    FdTableStore new_store(*store_.Lock());
    fbl::AllocChecker ac;
    fbl::RefPtr<FdTableInner> inner = fbl::MakeRefCountedChecked<FdTableInner>(&ac);
    ASSERT(ac.check());
    *inner->store_.Lock() = ktl::move(new_store);

    auto id = inner->id();
    for (auto& maybe_entry : inner->store_.Lock()->entries_) {
      if (maybe_entry.has_value()) {
        maybe_entry->fd_table_id_ = id;
      }
    }
    return inner;
  }

 private:
  friend class FdTable;

  mutable StarnixMutex<FdTableStore> store_;
};

class Task;
class FdTable {
 private:
  mutable StarnixMutex<fbl::RefPtr<FdTableInner>> inner_;

 public:
  // impl FdTable

  FdTableId id() const { return inner_.Lock()->get()->id(); }

  FdTable fork() const {
    auto inner = (*inner_.Lock())->unshare();
    return FdTable(inner);
  }

  void exec() const {
    retain([](FdNumber, FdFlags& flags) -> bool { return !flags.contains(FdFlagsEnum::CLOEXEC); });
  }

  fit::result<Errno, FdNumber> add_with_flags(const Task& task, FileHandle file,
                                              FdFlags flags) const;

  fit::result<Errno, FileHandle> get_allowing_opath(FdNumber fd) const;

  fit::result<Errno, ktl::pair<FileHandle, FdFlags>> get_allowing_opath_with_flags(
      FdNumber fd) const;

  fit::result<Errno, FileHandle> get(FdNumber fd) const;

  fit::result<Errno> close(FdNumber fd) const;

  fit::result<Errno, FdFlags> get_fd_flags(FdNumber fd) const;

  fit::result<Errno> set_fd_flags(FdNumber fd, FdFlags flags) const;

  void retain(std::function<bool(FdNumber, FdFlags&)> func) const;

  // Drop the fd table, closing any files opened exclusively by this table.
  void release() const { inner_.Lock()->reset(); }

  // C++
 public:
  static FdTable Create();

  FdTable(const FdTable& other) {
    *inner_.Lock() = fbl::RefPtr<FdTableInner>(other.inner_.Lock()->get());
  }

 private:
  FdTable(fbl::RefPtr<FdTableInner> table);
};

}  // namespace starnix

template <>
constexpr Flag<starnix::FdFlagsEnum> Flags<starnix::FdFlagsEnum>::FLAGS[] = {
    {starnix::FdFlagsEnum::CLOEXEC},
};

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_FD_TABLE_H_
