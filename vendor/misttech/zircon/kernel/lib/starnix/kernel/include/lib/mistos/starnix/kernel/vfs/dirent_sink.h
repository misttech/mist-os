// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_DIRECT_SINK_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_DIRECT_SINK_H_

#include <lib/mistos/linux_uapi/typedefs.h>
#include <lib/mistos/starnix/kernel/vfs/path.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/mistos/starnix_uapi/file_mode.h>
#include <zircon/types.h>

namespace starnix {

using starnix_uapi::Errno;
using starnix_uapi::FileMode;

class DirectoryEntryType {
 public:
  DirectoryEntryType() : bits_(0) {}
  explicit DirectoryEntryType(uint8_t bits) : bits_(bits) {}

  bool operator==(const DirectoryEntryType& other) const { return bits_ == other.bits_; }
  bool operator!=(const DirectoryEntryType& other) const { return !(*this == other); }

  static const DirectoryEntryType UNKNOWN;
  static const DirectoryEntryType FIFO;
  static const DirectoryEntryType CHR;
  static const DirectoryEntryType DIR;
  static const DirectoryEntryType BLK;
  static const DirectoryEntryType REG;
  static const DirectoryEntryType LNK;
  static const DirectoryEntryType SOCK;

  static DirectoryEntryType from_bits(uint8_t bits) { return DirectoryEntryType(bits); }
  static DirectoryEntryType from_mode(FileMode mode);

  uint8_t bits() const { return bits_; }

 private:
  uint8_t bits_;
};

inline DirectoryEntryType DirectoryEntryType::from_mode(FileMode mode) {
  switch (mode.fmt().bits()) {
    case S_IFLNK:
      return LNK;
    case S_IFREG:
      return REG;
    case S_IFDIR:
      return DIR;
    case S_IFCHR:
      return CHR;
    case S_IFBLK:
      return BLK;
    case S_IFIFO:
      return FIFO;
    case S_IFSOCK:
      return SOCK;
    default:
      return UNKNOWN;
  }
}

class DirentSink {
 public:
  virtual ~DirentSink() = default;

  /// Add the given directory entry to this buffer.
  ///
  /// In case of success, this will update the offset from the FileObject. Any other bookkeeping
  /// must be done by the caller after this method returns successfully.
  ///
  /// Returns error!(ENOSPC) if the entry does not fit.
  virtual fit::result<Errno> add(ino_t inode_num, off_t offset, DirectoryEntryType entry_type,
                                 const FsStr& name) = 0;

  /// The current offset to return.
  virtual off_t offset() const = 0;

  /// Optional information about the max number of bytes to send to the user.
  virtual ktl::optional<size_t> user_capacity() const { return ktl::nullopt; }
};

}  // namespace starnix

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_DIRECT_SINK_H_
