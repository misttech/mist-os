// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/vfs/fd_table.h"

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/task/kernel.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>
#include <lib/mistos/starnix/kernel/vfs/dir_entry.h>
#include <lib/mistos/starnix/kernel/vfs/file_object.h>
#include <lib/mistos/starnix/kernel/vfs/fs_node.h>
#include <lib/mistos/starnix_uapi/resource_limits.h>
#include <lib/mistos/util/error_propagation.h>
#include <trace.h>

#include <optional>
#include <utility>

#include <fbl/alloc_checker.h>
#include <ktl/optional.h>

#include "../kernel_priv.h"

#define LOCAL_TRACE STARNIX_KERNEL_GLOBAL_TRACE(0)

namespace starnix {

FdTableEntry::FdTableEntry(FileHandle file, FdTableId fd_table_id, FdFlags flags)
    : file_(ktl::move(file)), fd_table_id_(fd_table_id), flags_(flags) {}

FdTableEntry::~FdTableEntry() {
  LTRACEF_LEVEL(3, "fd_table_id %zx\n", fd_table_id_.id);
  auto fs = file_->name_->entry_->node_->fs();
  auto kernel = fs->kernel_.Lock();
  if (kernel) {
    // kernel->delayed_releaser.flush_file(file, fd_table_id_);
  }
}

FdTableStore::FdTableStore() = default;

FdTableStore::FdTableStore(const FdTableStore& other) {
  fbl::AllocChecker ac;
  entries_.reserve(other.entries_.capacity(), &ac);
  ASSERT(ac.check());

  ktl::copy(other.entries_.begin(), other.entries_.end(), util::back_inserter(entries_));
  next_fd_ = other.next_fd_;
}

FdTableStore& FdTableStore::operator=(FdTableStore&& other) = default;

fit::result<Errno, ktl::optional<FdTableEntry>> FdTableStore::insert_entry(FdNumber fd,
                                                                           uint64_t rlimit,
                                                                           FdTableEntry entry) {
  auto raw_fd = fd.raw();
  if (raw_fd < 0) {
    return fit::error(errno(EBADF));
  }
  if (static_cast<uint64_t>(raw_fd) >= rlimit) {
    return fit::error(errno(EMFILE));
  }
  if (raw_fd == next_fd_.raw()) {
    next_fd_ = calculate_lowest_available_fd(FdNumber::from_raw(raw_fd + 1));
  }
  auto raw_fd_size = static_cast<size_t>(raw_fd);
  if (raw_fd_size >= entries_.size()) {
    fbl::AllocChecker ac;
    entries_.resize(raw_fd_size + 1, &ac);
    if (!ac.check()) {
      return fit::error(errno(ENOMEM));
    }
  }

  auto local_entry = ktl::optional(entry);
  local_entry.swap(entries_[raw_fd_size]);
  return fit::ok(local_entry);
}

ktl::optional<FdTableEntry> FdTableStore::remove_entry(const FdNumber& fd) {
  LTRACEF("fd %d\n", fd.raw());
  auto raw_fd = static_cast<size_t>(fd.raw());
  if (entries_.is_empty() || raw_fd >= entries_.size()) {
    return ktl::nullopt;
  }
  auto removed = entries_[raw_fd];
  entries_[raw_fd] = ktl::nullopt;

  if (removed.has_value() && raw_fd < static_cast<size_t>(next_fd_.raw())) {
    next_fd_ = fd;
  }
  return removed;
}

ktl::optional<FdTableEntry> FdTableStore::get(FdNumber fd) const {
  LTRACEF("fd=%d, entries_.size=%zu\n", fd.raw(), entries_.size());
  if (entries_.is_empty() || static_cast<size_t>(fd.raw()) >= entries_.size()) {
    return ktl::nullopt;
  }
  return entries_[fd.raw()];
}

ktl::optional<std::reference_wrapper<ktl::optional<FdTableEntry>>> FdTableStore::get_mut(
    FdNumber fd) {
  LTRACEF("fd=%d, entries_.size=%zu\n", fd.raw(), entries_.size());
  if (entries_.is_empty() || static_cast<size_t>(fd.raw()) >= entries_.size()) {
    return ktl::nullopt;
  }
  return entries_[fd.raw()];
}

FdNumber FdTableStore::get_lowest_available_fd(FdNumber minfd) const {
  if (minfd.raw() > next_fd_.raw()) {
    return calculate_lowest_available_fd(minfd);
  }
  return next_fd_;
}

FdNumber FdTableStore::calculate_lowest_available_fd(const FdNumber& minfd) const {
  auto fd = minfd;
  while (get(fd).has_value()) {
    fd = FdNumber::from_raw(fd.raw() + 1);
  }
  return fd;
}

void FdTableStore::retain(std::function<bool(const FdNumber&, FdTableEntry&)> func) {
  for (size_t index = 0; index < entries_.size(); ++index) {
    auto fd = FdNumber::from_raw(static_cast<uint32_t>(index));
    auto& entry = entries_[index];
    if (entry.has_value()) {
      if (!func(fd, *entry)) {
        entry = ktl::nullopt;
      }
    }
  }
  next_fd_ = calculate_lowest_available_fd(FdNumber::from_raw(0));
}

FdTable::FdTable(fbl::RefPtr<FdTableInner> table) : inner_(ktl::move(table)) {}

FdTable FdTable::Create() {
  LTRACE;
  fbl::AllocChecker ac;
  fbl::RefPtr<FdTableInner> table = fbl::MakeRefCountedChecked<FdTableInner>(&ac);
  ASSERT(ac.check());
  return FdTable(table);
}

fit::result<Errno> FdTable::insert(const Task& task, FdNumber fd, FileHandle file) const {
  return insert_with_flags(task, fd, std::move(file), FdFlags::empty());
}

fit::result<Errno> FdTable::insert_with_flags(const Task& task, FdNumber fd, FileHandle file,
                                              FdFlags flags) const {
  auto rlimit = task.thread_group_->get_rlimit({ResourceEnum::NOFILE});
  auto id_ = id();
  auto inner = inner_.Lock();
  auto state = inner->get()->store_.Lock();
  auto result = state->insert_entry(fd, rlimit, {ktl::move(file), id_, flags}) _EP(result);
  return fit::ok();
}

fit::result<Errno, FdNumber> FdTable::add_with_flags(const Task& task, FileHandle file,
                                                     FdFlags flags) const {
  // profile_duration!("AddFd");
  auto rlimit = task.thread_group_->get_rlimit({ResourceEnum::NOFILE});
  auto id_ = id();
  auto inner = inner_.Lock();
  auto state = inner->get()->store_.Lock();
  auto fd = state->next_fd_;
  auto result = state->insert_entry(fd, rlimit, {ktl::move(file), id_, flags}) _EP(result);
  return fit::ok(fd);
}

fit::result<Errno, FdNumber> FdTable::duplicate(const Task& task, FdNumber oldfd,
                                                TargetFdNumber target, FdFlags flags) const {
  // profile_duration!("DuplicateFd");
  // Drop the removed entry only after releasing the writer lock in case
  // the close() function on the FileOps calls back into the FdTable.
  ktl::optional<FdTableEntry> _removed_entry;
  auto result = [&]() -> fit::result<Errno, FdNumber> {
    auto rlimit = task.thread_group_->get_rlimit({ResourceEnum::NOFILE});
    auto id_ = id();
    auto inner = inner_.Lock();
    auto state = inner->get()->store_.Lock();

    auto entry = state->get(oldfd);
    if (!entry.has_value()) {
      return fit::error(errno(EBADF));
    }
    auto file = entry->file_;

    auto fd =
        ktl::visit(TargetFdNumber::overloaded{
                       [&](const SpecificFd& s) -> fit::result<Errno, FdNumber> {
                         // We need to check the rlimit before we remove the entry from
                         // state because we cannot error out after removing the entry.
                         if (static_cast<uint64_t>(s.fd.raw()) >= rlimit) {
                           // ltp_dup201 shows that we're supposed to return EBADF in this
                           // situtation, instead of EMFILE, which is what we normally
                           // return when we're past the rlimit.
                           return fit::error(errno(EBADF));
                         }
                         _removed_entry = state->remove_entry(s.fd);
                         return fit::ok(s.fd);
                       },
                       [&](const MinimumFd& m) -> fit::result<Errno, FdNumber> {
                         return fit::ok(state->get_lowest_available_fd(m.fd));
                       },
                       [&](const ktl::monostate&) -> fit::result<Errno, FdNumber> {
                         return fit::ok(state->get_lowest_available_fd(FdNumber::from_raw(0)));
                       },
                   },
                   target.value_) _EP(fd);

    auto existing_entry =
        state->insert_entry(fd.value(), rlimit, {ktl::move(file), id_, flags}) _EP(existing_entry);
    ZX_ASSERT(!existing_entry.value().has_value());
    return fit::ok(fd.value());
  }();
  return result;
}

fit::result<Errno, FileHandle> FdTable::get_allowing_opath(FdNumber fd) const {
  auto result = get_allowing_opath_with_flags(fd) _EP(result);
  auto [file, _] = result.value();
  return fit::ok(file);
}

fit::result<Errno, ktl::pair<FileHandle, FdFlags>> FdTable::get_allowing_opath_with_flags(
    FdNumber fd) const {
  LTRACEF("fd %d\n", fd.raw());
  /*profile_duration!("GetFdWithFlags");*/
  auto inner = inner_.Lock();
  auto state = inner->get()->store_.Lock();
  auto entry = state->get(fd);
  if (entry.has_value()) {
    LTRACEF("has_value fd %d\n", fd.raw());
    return fit::ok(ktl::pair(entry->file_, entry->flags_));
  }
  return fit::error(errno(EBADF));
}

fit::result<Errno, FileHandle> FdTable::get(FdNumber fd) const {
  LTRACEF("fd %d\n", fd.raw());
  auto file = get_allowing_opath(fd) _EP(file);
  if (file->flags().contains(OpenFlagsEnum::PATH)) {
    LTRACEF("ERROR:constains OpenFlagsEnum::PATH\n");
    return fit::error(errno(EBADF));
  }
  return fit::ok(ktl::move(file.value()));
}

fit::result<Errno> FdTable::close(FdNumber fd) const {
  LTRACEF("fd %d\n", fd.raw());
  /*profile_duration!("CloseFile");*/
  // Drop the file object only after releasing the writer lock in case
  // the close() function on the FileOps calls back into the FdTable.
  auto removed = [&]() -> auto {
    auto inner = inner_.Lock();
    auto state = inner->get()->store_.Lock();
    return state->remove_entry(fd);
  }();

  if (removed.has_value()) {
    return fit::ok();
  }
  return fit::error(errno(EBADF));
}

fit::result<Errno, FdFlags> FdTable::get_fd_flags_allowing_opath(FdNumber fd) const {
  auto result = get_allowing_opath_with_flags(fd) _EP(result);
  return fit::ok(result->second);
}

fit::result<Errno, FdFlags> FdTable::get_fd_flags(FdNumber fd) const {
  auto result = get_allowing_opath_with_flags(fd);
  if (result.is_error()) {
    return result.take_error();
  }
  return fit::ok(result->second);
}

fit::result<Errno> FdTable::set_fd_flags(FdNumber fd, FdFlags flags) const {
  // profile_duration!("SetFdFlags");
  auto entry = (*(*inner_.Lock())->store_.Lock()).get_mut(fd);
  if (entry.has_value() && entry->get().has_value()) {
    entry->get()->flags_ = flags;
    return fit::ok();
  }
  return fit::error(errno(EBADF));
}

void FdTable::retain(std::function<bool(FdNumber, FdFlags&)> func) const {
  // profile_duration!("RetainFds");
  (*(*inner_.Lock())->store_.Lock())
      .retain([&func](const FdNumber& fd, FdTableEntry& entry) -> bool {
        return func(fd, entry.flags_);
      });
}

}  // namespace starnix
