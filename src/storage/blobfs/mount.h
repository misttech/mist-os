// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_STORAGE_BLOBFS_MOUNT_H_
#define SRC_STORAGE_BLOBFS_MOUNT_H_

#include <fidl/fuchsia.io/cpp/wire.h>
#include <fidl/fuchsia.process.lifecycle/cpp/wire.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/fit/function.h>
#include <lib/zx/resource.h>

#include <optional>

#include "src/lib/storage/block_client/cpp/block_device.h"
#include "src/storage/blobfs/cache_policy.h"
#include "src/storage/blobfs/compression/external_decompressor.h"
#include "src/storage/blobfs/compression_settings.h"

namespace blobfs {

using block_client::BlockDevice;

enum class Writability {
  // Do not write to persistent storage under any circumstances whatsoever.
  ReadOnlyDisk,
  // Do not allow users of the filesystem to mutate filesystem state. This state allows the journal
  // to replay while initializing writeback.
  ReadOnlyFilesystem,
  // Permit all operations.
  Writable,
};

// Toggles that may be set on blobfs during initialization.
struct MountOptions {
  Writability writability = Writability::Writable;
  bool verbose = false;
  CachePolicy cache_policy = CachePolicy::EvictImmediately;

  // Optional overriden cache policy for pager-backed blobs.
  std::optional<CachePolicy> pager_backed_cache_policy = std::nullopt;

  CompressionSettings compression_settings{};

  // Used to establish fidl connections to the DecompressorCreator instead of the default
  // implementation that will perform an |fdio_service_connect| with the given channel.
  DecompressorCreatorConnector* decompression_connector = nullptr;

  int paging_threads = 2;
#ifndef NDEBUG
  bool fsck_at_end_of_every_transaction = false;
#endif

  // Enable support for delivery blobs. Allows writing blobs in the format specified by RFC 0207.
  // If set, `streaming_writes` should also be enabled to reduce memory pressure when writing blobs.
  bool allow_delivery_blobs = false;
};

struct ComponentOptions {
  int pager_threads = 1;
};

// Start blobfs as a component. Begin serving requests on the provided |root|. Initially it starts
// the filesystem in an unconfigured state, only serving the fuchsia.fs.Startup protocol. Once
// fuchsia.fs.Startup/Start is called with the block device and mount options, the filesystem is
// started with that configuration and begins serving requests to other protocols, including the
// actual root of the filesystem at /root.
//
// Also expects a lifecycle server end over which to serve fuchsia.process.lifecycle/Lifecycle for
// shutting down the blobfs component.
//
// blobfs relies on the zx_vmo_replace_as_executable syscall to be able to serve executable blobs.
// The caller must either pass a valid Resource handle of kind ZX_RSRC_KIND_SYSTEM with base
// ZX_RSRC_SYSTEM_VMEX_BASE for |vmex_resource|, or else the mounted filesystem will not support
// requesting VMOs for blobs with VmoFlags::EXECUTE.
//
// This function blocks until the filesystem terminates.
zx::result<> StartComponent(ComponentOptions options, fidl::ServerEnd<fuchsia_io::Directory> root,
                            fidl::ServerEnd<fuchsia_process_lifecycle::Lifecycle> lifecycle,
                            zx::resource vmex_resource);

}  // namespace blobfs

#endif  // SRC_STORAGE_BLOBFS_MOUNT_H_
