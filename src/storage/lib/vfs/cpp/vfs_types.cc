// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/lib/vfs/cpp/vfs_types.h"

#include <fidl/fuchsia.io/cpp/natural_types.h>
#include <fidl/fuchsia.io/cpp/wire_types.h>
#include <lib/fit/function.h>

#include <type_traits>

namespace fio = fuchsia_io;

namespace fs {

VnodeConnectionOptions VnodeConnectionOptions::FromOpen1Flags(fio::OpenFlags open1_flags) {
  VnodeConnectionOptions options;
  // Filter out io1 OpenFlags.RIGHT_* flags, translated to io2 Rights below.
  options.flags = open1_flags & ~kAllIo1Rights;

  // Using Open1 requires GET_ATTRIBUTES as this is not expressible via |fio::OpenFlags|.
  // TODO(https://fxbug.dev/324080764): Restrict GET_ATTRIBUTES.
  options.rights = fio::Rights::kGetAttributes;

  // Approximate a set of io2 Rights corresponding to what is expected by |open1_flags|.
  if (!(options.flags & fio::OpenFlags::kNodeReference)) {
    if (open1_flags & fio::OpenFlags::kRightReadable) {
      options.rights |= fio::kRStarDir;
    }
    if (open1_flags & fio::OpenFlags::kRightWritable) {
      options.rights |= fio::kWStarDir;
    }
    if (open1_flags & fio::OpenFlags::kRightExecutable) {
      options.rights |= fio::kXStarDir;
    }
  }

  return options;
}

VnodeConnectionOptions VnodeConnectionOptions::FromCloneFlags(fio::OpenFlags clone_flags) {
  constexpr fio::OpenFlags kValidCloneFlags = kAllIo1Rights | fio::OpenFlags::kAppend |
                                              fio::OpenFlags::kDescribe |
                                              fio::OpenFlags::kCloneSameRights;
  // Any flags not present in |kValidCloneFlags| should be ignored.
  return FromOpen1Flags(clone_flags & kValidCloneFlags);
}

fio::OpenFlags VnodeConnectionOptions::ToIoV1Flags() const {
  return flags | RightsToOpenFlags(rights);
}

fio::OpenFlags RightsToOpenFlags(fio::Rights rights) {
  fio::OpenFlags flags = {};
  // Map io2 rights to io1 flags only if all constituent io2 rights are present.
  if ((rights & fio::kRStarDir) == fio::kRStarDir) {
    flags |= fio::OpenFlags::kRightReadable;
  }
  if ((rights & fio::kWStarDir) == fio::kWStarDir) {
    flags |= fio::OpenFlags::kRightWritable;
  }
  if ((rights & fio::kXStarDir) == fio::kXStarDir) {
    flags |= fio::OpenFlags::kRightExecutable;
  }
  return flags;
}

fio::wire::NodeAttributes VnodeAttributes::ToIoV1NodeAttributes() const {
  return fio::wire::NodeAttributes{.mode = mode,
                                   .id = inode,
                                   .content_size = content_size,
                                   .storage_size = storage_size,
                                   .link_count = link_count,
                                   .creation_time = creation_time,
                                   .modification_time = modification_time};
}

namespace internal {

bool ValidateCloneFlags(fio::OpenFlags flags) {
  // If CLONE_SAME_RIGHTS is specified, the client cannot request any specific rights.
  if (flags & fio::OpenFlags::kCloneSameRights) {
    return !(flags & kAllIo1Rights);
  }
  // All other flags are ignored.
  return true;
}

bool ValidateOpenFlags(fio::OpenFlags flags) {
  if ((flags & fio::OpenFlags::kNodeReference) &&
      (flags - fio::kOpenFlagsAllowedWithNodeReference)) {
    return false;
  }
  if ((flags & fio::OpenFlags::kNotDirectory) && (flags & fio::OpenFlags::kDirectory)) {
    return false;
  }
  if (flags & fio::OpenFlags::kCloneSameRights) {
    return false;
  }
  return true;
}

}  // namespace internal

}  // namespace fs
