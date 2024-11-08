// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_LIB_ELFLDLTL_INCLUDE_LIB_ELFLDLTL_FD_H_
#define SRC_LIB_ELFLDLTL_INCLUDE_LIB_ELFLDLTL_FD_H_

#include <lib/fit/result.h>
#include <unistd.h>

#include <cerrno>
#include <span>

#include <fbl/unique_fd.h>

#include "file.h"
#include "posix.h"

namespace elfldltl {

// elfldltl::UniqueFdFile is constructible from fbl::unique_fd and meets the
// File API (see <lib/elfldltl/memory.h>) by calling pread.  The same thing
// for unowned plain `int` file descriptors is provided by elfldltl::FdFile.

namespace internal {

inline fit::result<PosixError> ReadFd(int fd, off_t offset, std::span<std::byte> buffer) {
  do {
    ssize_t n = pread(fd, buffer.data(), buffer.size(), offset);
    if (n < 0) {
      return fit::error{PosixError{errno}};
    }
    if (n == 0) {
      return fit::error{PosixError{}};  // This signifies EOF.
    }
    buffer = buffer.subspan(static_cast<size_t>(n));
    offset += n;
  } while (!buffer.empty());
  return fit::ok();
}

inline int MakeInvalidFd() { return -1; }

inline fit::result<PosixError> ReadUniqueFd(const fbl::unique_fd& fd, off_t offset,
                                            std::span<std::byte> buffer) {
  return ReadFd(fd.get(), offset, buffer);
}

}  // namespace internal

template <class Diagnostics>
using FdFileBase = File<Diagnostics, int, off_t, internal::ReadFd, internal::MakeInvalidFd>;

template <class Diagnostics>
using UniqueFdFileBase = File<Diagnostics, fbl::unique_fd, off_t, internal::ReadUniqueFd>;

template <class Diagnostics>
class FdFile : public FdFileBase<Diagnostics> {
 public:
  using FdFileBase<Diagnostics>::FdFileBase;

  FdFile(const FdFile&) noexcept = default;

  FdFile(FdFile&&) noexcept = default;

  FdFile& operator=(const FdFile&) noexcept = default;

  FdFile& operator=(FdFile&&) noexcept = default;

  int borrow() const { return this->get(); }
};

// Deduction guide.
template <class Diagnostics>
FdFile(int fd, Diagnostics& diagnostics) -> FdFile<Diagnostics>;

template <class Diagnostics>
FdFile(Diagnostics& diagnostics) -> FdFile<Diagnostics>;

template <class Diagnostics>
class UniqueFdFile : public UniqueFdFileBase<Diagnostics> {
 public:
  using Base = UniqueFdFileBase<Diagnostics>;

  using Base::Base;

  UniqueFdFile(UniqueFdFile&&) noexcept = default;

  UniqueFdFile& operator=(UniqueFdFile&&) noexcept = default;

  int get() const { return Base::get().get(); }

  int borrow() const { return get(); }
};

// Deduction guide.
template <class Diagnostics>
UniqueFdFile(fbl::unique_fd fd, Diagnostics& diagnostics) -> UniqueFdFile<Diagnostics>;

template <class Diagnostics>
UniqueFdFile(Diagnostics& diagnostics) -> UniqueFdFile<Diagnostics>;

}  // namespace elfldltl

#endif  // SRC_LIB_ELFLDLTL_INCLUDE_LIB_ELFLDLTL_FD_H_
