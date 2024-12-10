// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_FS_NODE_INFO_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_FS_NODE_INFO_H_

#include <lib/mistos/starnix_uapi/auth.h>
#include <lib/mistos/starnix_uapi/device_type.h>
#include <lib/mistos/starnix_uapi/file_mode.h>

namespace starnix {

/// `st_blksize` is measured in units of 512 bytes.
const size_t DEFAULT_BYTES_PER_BLOCK = 512;

struct FsNodeInfo {
  ino_t ino_;
  starnix_uapi::FileMode mode_;
  size_t link_count_;
  uid_t uid_;
  gid_t gid_;
  starnix_uapi::DeviceType rdev_;
  size_t size_;
  size_t blksize_;
  size_t blocks_;
  // pub time_status_change: zx::Time,
  // pub time_access: zx::Time,
  // pub time_modify: zx::Time,
  // pub sid: Option<SecurityId>,

  /// impl FsNodeInfo
  static FsNodeInfo New(ino_t ino, starnix_uapi::FileMode mode, starnix_uapi::FsCred owner) {
    return {
        .ino_ = ino,
        .mode_ = mode,
        .uid_ = owner.uid_,
        .gid_ = owner.gid_,
        .rdev_ = starnix_uapi::DeviceType(0),
        .size_ = 0,
        .blksize_ = DEFAULT_BYTES_PER_BLOCK,
        .blocks_ = 0,
    };
  }

  size_t storage_size() const {
    // TODO (Herrera) : saturating_mul
    return blksize_ * blocks_;
  }

  static std::function<FsNodeInfo(ino_t)> new_factory(starnix_uapi::FileMode mode,
                                                      starnix_uapi::FsCred owner) {
    return [mode, owner](ino_t ino) -> FsNodeInfo { return FsNodeInfo::New(ino, mode, owner); };
  }

  void chmod(const starnix_uapi::FileMode& mode) {
    mode_ = (mode_ & ~starnix_uapi::FileMode::PERMISSIONS) |
            (mode & starnix_uapi::FileMode::PERMISSIONS);
    // self.time_status_change = utc::utc_now();
  }
};

}  // namespace starnix

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_FS_NODE_INFO_H_
