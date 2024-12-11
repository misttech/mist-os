// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_FS_NODE_INFO_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_FS_NODE_INFO_H_

#include <lib/mistos/starnix_uapi/auth.h>
#include <lib/mistos/starnix_uapi/device_type.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/mistos/starnix_uapi/file_mode.h>

namespace starnix {

using starnix_uapi::Access;
using starnix_uapi::AccessEnum;
using starnix_uapi::Credentials;
using starnix_uapi::DeviceType;
using starnix_uapi::Errno;
using starnix_uapi::FileMode;
using starnix_uapi::FsCred;
using starnix_uapi::UserAndOrGroupId;

/// `st_blksize` is measured in units of 512 bytes.
const size_t DEFAULT_BYTES_PER_BLOCK = 512;

struct FsNodeInfo {
  ino_t ino_;
  FileMode mode_;
  size_t link_count_;
  uid_t uid_;
  gid_t gid_;
  DeviceType rdev_;
  size_t size_;
  size_t blksize_;
  size_t blocks_;
  // pub time_status_change: zx::Time,
  // pub time_access: zx::Time,
  // pub time_modify: zx::Time,
  // pub sid: Option<SecurityId>,

  /// impl FsNodeInfo
  static FsNodeInfo New(ino_t ino, FileMode mode, FsCred owner) {
    return {
        .ino_ = ino,
        .mode_ = mode,
        .uid_ = owner.uid_,
        .gid_ = owner.gid_,
        .rdev_ = DeviceType(0),
        .size_ = 0,
        .blksize_ = DEFAULT_BYTES_PER_BLOCK,
        .blocks_ = 0,
    };
  }

  size_t storage_size() const {
    // TODO (Herrera) : saturating_mul
    return blksize_ * blocks_;
  }

  static std::function<FsNodeInfo(ino_t)> new_factory(FileMode mode, FsCred owner) {
    return [mode, owner](ino_t ino) -> FsNodeInfo { return FsNodeInfo::New(ino, mode, owner); };
  }

  void chmod(const FileMode& mode) {
    mode_ = (mode_ & ~FileMode::PERMISSIONS) | (mode & FileMode::PERMISSIONS);
    // self.time_status_change = utc::utc_now();
  }

  void chown(ktl::optional<uid_t> owner, ktl::optional<gid_t> group) {
    if (owner) {
      uid_ = *owner;
    }
    if (group) {
      gid_ = *group;
    }
    // Clear the setuid and setgid bits if the file is executable and a regular file.
    if (mode_.is_reg()) {
      mode_ &= ~FileMode::ISUID;
      clear_sgid_bit();
    }
    // self.time_status_change = utc::utc_now();
  }

  void clear_sgid_bit() {
    // If the group execute bit is not set, the setgid bit actually indicates mandatory
    // locking and should not be cleared.
    if (mode_.intersects(FileMode::IXGRP)) {
      mode_ &= ~FileMode::ISGID;
    }
  }

  void clear_suid_and_sgid_bits() {
    mode_ &= ~FileMode::ISUID;
    clear_sgid_bit();
  }

  starnix_uapi::FsCred cred() const { return starnix_uapi::FsCred{.uid_ = uid_, .gid_ = gid_}; }

  fit::result<Errno, UserAndOrGroupId> suid_and_sgid(const Credentials& creds) const {
    ktl::optional<uid_t> uid;
    if (mode_.contains(FileMode::ISUID)) {
      uid = uid_;
    }

    // See <https://man7.org/linux/man-pages/man7/inode.7.html>:
    //
    //   For an executable file, the set-group-ID bit causes the
    //   effective group ID of a process that executes the file to change
    //   as described in execve(2).  For a file that does not have the
    //   group execution bit (S_IXGRP) set, the set-group-ID bit indicates
    //   mandatory file/record locking.
    ktl::optional<gid_t> gid;
    if (mode_.contains(FileMode::ISGID | FileMode::IXGRP)) {
      gid = gid_;
    }

    UserAndOrGroupId maybe_set_id{.uid_ = uid, .gid_ = gid};
    if (maybe_set_id.is_some()) {
      // Check that uid and gid actually have execute access before
      // returning them as the SUID or SGID.
      _EP(creds.check_access(Access(AccessEnum::EXEC), uid_, gid_, mode_));
    }
    return fit::ok(maybe_set_id);
  }
};

}  // namespace starnix

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_FS_NODE_INFO_H_
