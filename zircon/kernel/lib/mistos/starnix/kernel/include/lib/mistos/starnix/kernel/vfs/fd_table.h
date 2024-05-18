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
#include <lib/mistos/util/bitflags.h>
#include <zircon/compiler.h>

#include <atomic>
#include <utility>
#include <vector>

#include <fbl/ref_counted.h>
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
  FdTableEntry(FileHandle _file, FdTableId fd_table_id, FdFlags flags)
      : file(std::move(_file)), fd_table_id_(fd_table_id), flags_(flags) {}
  ~FdTableEntry();

  FileHandle file;

 private:
  friend class FdTable;

  // Identifier of the FdTable containing this entry.
  FdTableId fd_table_id_;

  // Rather than using a separate "flags" field, we could maintain this data
  // as a bitfield over the file descriptors because there is only one flag
  // currently (CLOEXEC) and file descriptor numbers tend to cluster near 0.
  FdFlags flags_;
};

/// Having the map a separate data structure allows us to memoize next_fd, which is the
/// lowest numbered file descriptor not in use.
class FdTableStore {
 public:
  fit::result<Errno, ktl::optional<FdTableEntry>> insert_entry(FdNumber fd, uint64_t rlimit,
                                                               FdTableEntry entry);

  ktl::optional<FdTableEntry> remove_entry(const FdNumber& fd);

  ktl::optional<FdTableEntry> get(FdNumber fd) const;

  // Recalculates the lowest available FD >= minfd based on the contents of the map.
  FdNumber calculate_lowest_available_fd(FdNumber minfd) const;

 private:
  friend class FdTableInner;
  friend class FdTable;

  std::vector<ktl::optional<FdTableEntry>> entries_ = {};
  FdNumber next_fd_ = FdNumber::from_raw(0);
};

class FdTableInner : public fbl::RefCounted<FdTableInner> {
 public:
  FdTableId id() const { return {reinterpret_cast<size_t>(&store_.Lock()->entries_)}; }

 private:
  friend class FdTable;

  mutable StarnixMutex<FdTableStore> store_;
};

class Task;
class FdTable {
 public:
  static FdTable Create();
  FdTable(const FdTable& other) { inner_.Lock()->swap(*other.inner_.Lock()); }

  FdTableId id() const { return inner_.Lock()->get()->id(); }

  fit::result<Errno, FdNumber> add_with_flags(const Task& task, FileHandle file,
                                              FdFlags flags) const;

  fit::result<Errno, FileHandle> get_allowing_opath(FdNumber fd) const;

  fit::result<Errno, std::pair<FileHandle, FdFlags>> get_allowing_opath_with_flags(
      FdNumber fd) const;

  fit::result<Errno, FileHandle> get(FdNumber fd) const;

  fit::result<Errno> close(FdNumber fd) const;

  // Drop the fd table, closing any files opened exclusively by this table.
  void release() const { inner_.Lock() = fbl::RefPtr<FdTableInner>(); }

 private:
  FdTable(fbl::RefPtr<FdTableInner> table);

  mutable StarnixMutex<fbl::RefPtr<FdTableInner>> inner_;
};

}  // namespace starnix

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_FD_TABLE_H_
