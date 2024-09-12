// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_MOUNT_INFO_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_MOUNT_INFO_H_

#include <lib/fit/result.h>
#include <lib/mistos/starnix_uapi/errors.h>
#include <lib/mistos/starnix_uapi/mount_flags.h>

#include <fbl/ref_ptr.h>
#include <ktl/optional.h>

namespace starnix {

class Mount;
using MountHandle = fbl::RefPtr<Mount>;
using MountFlags = starnix_uapi::MountFlags;

/// Public representation of the mount options.
struct MountInfo {
  ktl::optional<MountHandle> handle;

 public:
  // impl MountInfo
  /// `MountInfo` for a element that is not tied to a given mount. Mount flags will be considered
  /// empty.
  static MountInfo detached();

  /// The mount flags of the represented mount.
  MountFlags flags();

  /// Checks whether this `MountInfo` represents a writable file system mounted.
  fit::result<Errno> check_readonly_filesystem();

 public:
  // C++
  ktl::optional<MountHandle> operator*() const;

  ~MountInfo();
};

}  // namespace starnix

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_MOUNT_INFO_H_
