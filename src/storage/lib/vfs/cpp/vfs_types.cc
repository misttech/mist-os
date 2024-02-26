// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/lib/vfs/cpp/vfs_types.h"

#include <fidl/fuchsia.io/cpp/wire.h>
#include <lib/fdio/vfs.h>
#include <lib/fit/function.h>

namespace fio = fuchsia_io;

namespace fs {

VnodeConnectionOptions VnodeConnectionOptions::FromIoV1Flags(
    fuchsia_io::wire::OpenFlags fidl_flags) {
  VnodeConnectionOptions options;

  // Flags:
  if (fidl_flags & fio::wire::OpenFlags::kCreate) {
    options.flags.create = true;
  }
  if (fidl_flags & fio::wire::OpenFlags::kCreateIfAbsent) {
    options.flags.fail_if_exists = true;
  }
  if (fidl_flags & fio::wire::OpenFlags::kTruncate) {
    options.flags.truncate = true;
  }
  if (fidl_flags & fio::wire::OpenFlags::kDirectory) {
    options.flags.directory = true;
  }
  if (fidl_flags & fio::wire::OpenFlags::kAppend) {
    options.flags.append = true;
  }
  if (fidl_flags & fio::wire::OpenFlags::kNodeReference) {
    options.flags.node_reference = true;
  }
  if (fidl_flags & fio::wire::OpenFlags::kDescribe) {
    options.flags.describe = true;
  }
  if (fidl_flags & fio::wire::OpenFlags::kPosixWritable) {
    options.flags.posix_write = true;
  }
  if (fidl_flags & fio::wire::OpenFlags::kPosixExecutable) {
    options.flags.posix_execute = true;
  }
  if (fidl_flags & fio::wire::OpenFlags::kNotDirectory) {
    options.flags.not_directory = true;
  }
  if (fidl_flags & fio::wire::OpenFlags::kCloneSameRights) {
    options.flags.clone_same_rights = true;
  }

  // Rights (these are smushed into |fidl_flags| in fuchsia.io v1):
  if (fidl_flags & fio::wire::OpenFlags::kRightReadable) {
    options.rights.read = true;
  }
  if (fidl_flags & fio::wire::OpenFlags::kRightWritable) {
    options.rights.write = true;
  }
  if (fidl_flags & fio::wire::OpenFlags::kRightExecutable) {
    options.rights.execute = true;
  }

  return options;
}

fuchsia_io::wire::OpenFlags VnodeConnectionOptions::ToIoV1Flags() const {
  fuchsia_io::wire::OpenFlags fidl_flags = {};

  // Flags:
  if (flags.create) {
    fidl_flags |= fio::wire::OpenFlags::kCreate;
  }
  if (flags.fail_if_exists) {
    fidl_flags |= fio::wire::OpenFlags::kCreateIfAbsent;
  }
  if (flags.truncate) {
    fidl_flags |= fio::wire::OpenFlags::kTruncate;
  }
  if (flags.directory) {
    fidl_flags |= fio::wire::OpenFlags::kDirectory;
  }
  if (flags.append) {
    fidl_flags |= fio::wire::OpenFlags::kAppend;
  }
  if (flags.node_reference) {
    fidl_flags |= fio::wire::OpenFlags::kNodeReference;
  }
  if (flags.describe) {
    fidl_flags |= fio::wire::OpenFlags::kDescribe;
  }
  if (flags.posix_write) {
    fidl_flags |= fio::wire::OpenFlags::kPosixWritable;
  }
  if (flags.posix_execute) {
    fidl_flags |= fio::wire::OpenFlags::kPosixExecutable;
  }
  if (flags.not_directory) {
    fidl_flags |= fio::wire::OpenFlags::kNotDirectory;
  }
  if (flags.clone_same_rights) {
    fidl_flags |= fio::wire::OpenFlags::kCloneSameRights;
  }

  // Rights (these are smushed into |fidl_flags| in fuchsia.io v1):
  if (rights.read) {
    fidl_flags |= fio::wire::OpenFlags::kRightReadable;
  }
  if (rights.write) {
    fidl_flags |= fio::wire::OpenFlags::kRightWritable;
  }
  if (rights.execute) {
    fidl_flags |= fio::wire::OpenFlags::kRightExecutable;
  }

  return fidl_flags;
}

VnodeConnectionOptions VnodeConnectionOptions::FilterForNewConnection(
    VnodeConnectionOptions options) {
  VnodeConnectionOptions result;
  result.flags.append = options.flags.append;
  result.flags.node_reference = options.flags.node_reference;
  result.rights = options.rights;
  return result;
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

fuchsia_io::wire::NodeInfoDeprecated ConvertToIoV1NodeInfo(
    fidl::AnyArena& arena, fuchsia_io::Representation representation) {
  switch (representation.Which()) {
    case fuchsia_io::Representation::Tag::kConnector:
      return fuchsia_io::wire::NodeInfoDeprecated::WithService({});
    case fuchsia_io::Representation::Tag::kDirectory:
      return fuchsia_io::wire::NodeInfoDeprecated::WithDirectory({});
    case fuchsia_io::Representation::Tag::kFile: {
      auto info = fuchsia_io::wire::NodeInfoDeprecated::WithFile(arena);
      if (representation.file()->observer()) {
        info.file().event = std::move(*representation.file()->observer());
      }
      if (representation.file()->stream()) {
        info.file().stream = std::move(*representation.file()->stream());
      }
      return info;
    }
    default:
      ZX_PANIC("Protocol not supported by fuchsia.io1.");
  }
}

}  // namespace fs
