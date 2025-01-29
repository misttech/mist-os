// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_STORAGE_LIB_FS_MANAGEMENT_CPP_OPTIONS_H_
#define SRC_STORAGE_LIB_FS_MANAGEMENT_CPP_OPTIONS_H_

#include <fidl/fuchsia.fs.startup/cpp/wire.h>
#include <fidl/fuchsia.fxfs/cpp/wire.h>
#include <fidl/fuchsia.io/cpp/wire.h>
#include <lib/zx/result.h>
#include <zircon/types.h>

#include <cstdint>
#include <vector>

namespace fs_management {

struct MountOptions {
  bool readonly = false;
  bool verbose_mount = false;

  // Ensures that requests to the mountpoint will be propagated to the underlying FS
  bool wait_until_ready = true;

  // An optional compression algorithm specifier for the filesystem to use when storing files (if
  // the filesystem supports it).
  std::optional<std::string> write_compression_algorithm;

  // An optional compression level for the filesystem to use when storing files (if the filesystem
  // and the configured |write_compression_algorithm| supports it).
  // Setting to < 0 indicates no value (the filesystem chooses a default if necessary).
  int write_compression_level = -1;

  // An optional cache eviction policy specifier for the filesystem to use for in-memory data (if
  // the filesystem supports it).
  std::optional<std::string> cache_eviction_policy;

  // If set, run fsck after every transaction.
  bool fsck_after_every_transaction = false;

  // If set, a callable that connects and returns a handle to the crypt service.
  std::function<zx::result<fidl::ClientEnd<fuchsia_fxfs::Crypt>>()> crypt_client;

  // Generate a StartOptions fidl struct to pass the a fuchsia.fs.startup.Startup interface based
  // on this set of options.
  __EXPORT
  zx::result<fuchsia_fs_startup::wire::StartOptions> as_start_options() const;
};

struct MkfsOptions {
  uint32_t fvm_data_slices = 1;
  bool verbose = false;

  // The number of sectors per cluster on a FAT file systems or zero for the default.
  uint16_t sectors_per_cluster = 0;

  // Set to use the deprecated padded blobfs format.
  bool deprecated_padded_blobfs_format = false;

  // The initial number of inodes to allocate space for. If 0, a default is used. Only supported
  // for blobfs.
  uint64_t num_inodes = 0;

  // Generate a FormatOptions fidl struct to pass the a fuchsia.fs.startup.Startup interface based
  // on this set of options.
  __EXPORT
  fuchsia_fs_startup::wire::FormatOptions as_format_options(fidl::AnyArena& arena) const;
};

struct FsckOptions {
  bool verbose = false;

  // At MOST one of the following '*_modify' flags may be true.
  bool never_modify = false;   // Fsck still looks for problems, but does not try to resolve them.
  bool always_modify = false;  // Fsck never asks to resolve problems; it will always do it.
  bool force = false;          // Force fsck to check the filesystem integrity, even if "clean".

  // Generate a CheckOptions fidl struct to pass the a fuchsia.fs.startup.Startup interface based
  // on this set of options.
  //
  // The current set of filesystems that support launching with fuchsia.fs.startup.Startup don't
  // support any check options so this doesn't currently do anything. This function is provided for
  // consistency.
  __EXPORT
  fuchsia_fs_startup::wire::CheckOptions as_check_options() const;
};

}  // namespace fs_management

#endif  // SRC_STORAGE_LIB_FS_MANAGEMENT_CPP_OPTIONS_H_
