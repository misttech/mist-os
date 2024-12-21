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

#include <linux/fs.h>

namespace starnix {

using starnix_uapi::Errno;
using starnix_uapi::MountFlags;

class Mount;
using MountHandle = fbl::RefPtr<Mount>;

enum class RenameFlagsEnum : uint32_t {
  // Exchange the entries.
  EXCHANGE = RENAME_EXCHANGE,

  // Don't overwrite an existing DirEntry.
  NOREPLACE = RENAME_NOREPLACE,

  // Create a "whiteout" object to replace the file.
  WHITEOUT = RENAME_WHITEOUT,

  // Allow replacing any file with a directory. This is an internal flag used only
  // internally inside Starnix for OverlayFS.
  REPLACE_ANY = 0x80000000,

  // Internal flags that cannot be passed to `sys_rename()`
  INTERNAL = REPLACE_ANY
};

using RenameFlags = Flags<RenameFlagsEnum>;

/// Public representation of the mount options.
struct MountInfo {
 public:
  ktl::optional<MountHandle> handle_;

  // impl MountInfo
  /// `MountInfo` for a element that is not tied to a given mount. Mount flags will be considered
  /// empty.
  static MountInfo detached();

  /// The mount flags of the represented mount.
  MountFlags flags() const;

  /// Checks whether this `MountInfo` represents a writable file system mounted.
  fit::result<Errno> check_readonly_filesystem() const;

  /// Checks whether this `MountInfo` represents an executable file system mount.
  fit::result<Errno> check_noexec_filesystem() const;

  // C++
  ktl::optional<MountHandle> operator*() const;

  bool operator==(const MountInfo& other) const { return handle_ == other.handle_; }

  bool operator!=(const MountInfo& other) const { return !(*this == other); }

  ~MountInfo();
};

}  // namespace starnix

template <>
constexpr Flag<starnix::RenameFlagsEnum> Flags<starnix::RenameFlagsEnum>::FLAGS[] = {
    {starnix::RenameFlagsEnum::EXCHANGE}, {starnix::RenameFlagsEnum::NOREPLACE},
    {starnix::RenameFlagsEnum::WHITEOUT}, {starnix::RenameFlagsEnum::REPLACE_ANY},
    {starnix::RenameFlagsEnum::INTERNAL},
};

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_VFS_MOUNT_INFO_H_
