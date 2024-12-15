// Copyright 2021 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_FD_TABLE_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_FD_TABLE_H_

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/vfs/fd_number.h>
#include <lib/mistos/util/back_insert_iterator.h>
#include <lib/mistos/util/bitflags.h>
#include <lib/starnix_sync/locks.h>
#include <zircon/compiler.h>

#include <functional>

#include <fbl/ref_counted.h>
#include <fbl/vector.h>
#include <ktl/algorithm.h>
#include <ktl/move.h>
#include <ktl/optional.h>

#include <linux/fcntl.h>

namespace starnix {

enum class FdFlagsEnum : uint8_t {
  CLOEXEC = FD_CLOEXEC,
};

using FdFlags = Flags<FdFlagsEnum>;

struct FdTableId {
  size_t id;
};

class FileObject;
using FileHandle = fbl::RefPtr<FileObject>;

struct FdTableEntry {
 public:
  FileHandle file_;

 private:
  // Identifier of the FdTable containing this entry.
  FdTableId fd_table_id_;

  // Rather than using a separate "flags" field, we could maintain this data
  // as a bitfield over the file descriptors because there is only one flag
  // currently (CLOEXEC) and file descriptor numbers tend to cluster near 0.
  FdFlags flags_;

 public:
  FdTableEntry(FileHandle file, FdTableId fd_table_id, FdFlags flags);

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

  // Returns the (possibly memoized) lowest available FD >= minfd in this map.
  FdNumber get_lowest_available_fd(FdNumber minfd) const;

  // Recalculates the lowest available FD >= minfd based on the contents of the map.
  FdNumber calculate_lowest_available_fd(const FdNumber& minfd) const;

  void retain(std::function<bool(const FdNumber&, FdTableEntry&)> func);

  FdTableStore();
  FdTableStore(const FdTableStore& other);
  FdTableStore& operator=(FdTableStore&& other);

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

  mutable starnix_sync::Mutex<FdTableStore> store_;
};

struct SpecificFd {
  FdNumber fd;
};

struct MinimumFd {
  FdNumber fd;
};

class TargetFdNumber {
 public:
  /// The duplicated FdNumber will be the smallest available FdNumber.
  static TargetFdNumber Default() { return TargetFdNumber(); }

  /// The duplicated FdNumber should be this specific FdNumber.
  static TargetFdNumber Specific(FdNumber fd) { return TargetFdNumber(SpecificFd{fd}); }

  /// The duplicated FdNumber should be greater than this FdNumber.
  static TargetFdNumber Minimum(FdNumber fd) { return TargetFdNumber(MinimumFd{fd}); }

 private:
  using Value = ktl::variant<ktl::monostate, SpecificFd, MinimumFd>;

  // Helper for variant visitors
  template <typename... Ts>
  struct overloaded : Ts... {
    using Ts::operator()...;
  };
  template <typename... Ts>
  overloaded(Ts...) -> overloaded<Ts...>;

  friend class FdTable;

  /// The duplicated FdNumber will be the smallest available FdNumber.
  TargetFdNumber() : value_(ktl::monostate{}) {}

  explicit TargetFdNumber(Value value) : value_(value) {}

  Value value_;
};

class Task;
class FdTable {
 private:
  mutable starnix_sync::Mutex<fbl::RefPtr<FdTableInner>> inner_;

 public:
  /// impl FdTable
  FdTableId id() const { return inner_.Lock()->get()->id(); }

  FdTable fork() const {
    auto inner = (*inner_.Lock())->unshare();
    return FdTable(inner);
  }

  void exec() const {
    retain([](FdNumber, FdFlags& flags) -> bool { return !flags.contains(FdFlagsEnum::CLOEXEC); });
  }

  fit::result<Errno> insert(const Task& task, FdNumber fd, FileHandle file) const;

  fit::result<Errno> insert_with_flags(const Task& task, FdNumber fd, FileHandle file,
                                       FdFlags flags) const;

  fit::result<Errno, FdNumber> add_with_flags(const Task& task, FileHandle file,
                                              FdFlags flags) const;

  fit::result<Errno, FdNumber> duplicate(const Task& task, FdNumber oldfd, TargetFdNumber target,
                                         FdFlags flags) const;

  fit::result<Errno, FileHandle> get_allowing_opath(FdNumber fd) const;

  fit::result<Errno, ktl::pair<FileHandle, FdFlags>> get_allowing_opath_with_flags(
      FdNumber fd) const;

  fit::result<Errno, FileHandle> get(FdNumber fd) const;

  fit::result<Errno> close(FdNumber fd) const;

  fit::result<Errno, FdFlags> get_fd_flags_allowing_opath(FdNumber fd) const;

  fit::result<Errno> set_fd_flags(FdNumber fd, FdFlags flags) const;

  fit::result<Errno> set_fd_flags_allowing_opath(FdNumber fd, FdFlags flags) const;

  void retain(std::function<bool(FdNumber, FdFlags&)> func) const;

  /// impl ReleasableByRef for FdTable

  ///  Drop the fd table, closing any files opened exclusively by this table.
  void release() const { inner_.Lock()->reset(); }

  // C++
  static FdTable Create();

  FdTable(const FdTable& other) {
    *inner_.Lock() = fbl::RefPtr<FdTableInner>(other.inner_.Lock()->get());
  }

 private:
  explicit FdTable(fbl::RefPtr<FdTableInner> table);
};

}  // namespace starnix

template <>
constexpr Flag<starnix::FdFlagsEnum> Flags<starnix::FdFlagsEnum>::FLAGS[] = {
    {starnix::FdFlagsEnum::CLOEXEC},
};

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_FD_TABLE_H_
