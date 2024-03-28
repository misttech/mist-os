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

VnodeConnectionOptions VnodeConnectionOptions::FromIoV1Flags(fio::OpenFlags fidl_flags) {
  VnodeConnectionOptions options;
  // Filter out io1 OpenFlags.RIGHT_* flags, translated to io2 Rights below.
  options.flags = fidl_flags & ~kAllIo1Rights;

  // Using Open1 requires GET_ATTRIBUTES as this is not expressible via |fio::OpenFlags|.
  // TODO(https://fxbug.dev/324080764): Restrict GET_ATTRIBUTES.
  options.rights = fio::Rights::kGetAttributes;

  // Approximate a set of io2 Rights corresponding to what is expected by |fidl_flags|.
  if (!(options.flags & fio::OpenFlags::kNodeReference)) {
    if (fidl_flags & fio::OpenFlags::kRightReadable) {
      options.rights |= fio::kRStarDir;
    }
    if (fidl_flags & fio::OpenFlags::kRightWritable) {
      options.rights |= fio::kWStarDir;
    }
    if (fidl_flags & fio::OpenFlags::kRightExecutable) {
      options.rights |= fio::kXStarDir;
    }
  }

  return options;
}

fio::OpenFlags VnodeConnectionOptions::ToIoV1Flags() const {
  fio::OpenFlags fidl_flags = flags;
  // Map io2 rights to io1 flags only if all constituent io2 rights are present.
  if ((rights & fio::kRStarDir) == fio::kRStarDir) {
    fidl_flags |= fio::OpenFlags::kRightReadable;
  }
  if ((rights & fio::kWStarDir) == fio::kWStarDir) {
    fidl_flags |= fio::OpenFlags::kRightWritable;
  }
  if ((rights & fio::kXStarDir) == fio::kXStarDir) {
    fidl_flags |= fio::OpenFlags::kRightExecutable;
  }
  return fidl_flags;
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

}  // namespace fs
