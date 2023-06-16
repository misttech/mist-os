// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/blobfs/service/startup.h"

#include <lib/syslog/cpp/macros.h>

#include "src/lib/storage/block_client/cpp/remote_block_device.h"
#include "src/storage/blobfs/fsck.h"
#include "src/storage/blobfs/mkfs.h"
#include "src/storage/blobfs/mount.h"

namespace blobfs {
namespace {

MountOptions ParseMountOptions(fuchsia_fs_startup::wire::StartOptions start_options) {
  MountOptions options;

  options.verbose = start_options.verbose;
  options.allow_delivery_blobs = start_options.allow_delivery_blobs;

  if (start_options.read_only) {
    options.writability = Writability::ReadOnlyFilesystem;
  }
  if (start_options.write_compression_level >= 0) {
    options.compression_settings.compression_level = start_options.write_compression_level;
  }

  switch (start_options.write_compression_algorithm) {
    case fuchsia_fs_startup::wire::CompressionAlgorithm::kZstdChunked:
      options.compression_settings.compression_algorithm = CompressionAlgorithm::kChunked;
      break;
    case fuchsia_fs_startup::wire::CompressionAlgorithm::kUncompressed:
      options.compression_settings.compression_algorithm = CompressionAlgorithm::kUncompressed;
      break;
    default:
      ZX_PANIC("Unknown compression algorithm: %d",
               static_cast<uint32_t>(start_options.write_compression_algorithm));
  }

  switch (start_options.cache_eviction_policy_override) {
    case fuchsia_fs_startup::wire::EvictionPolicyOverride::kNone:
      options.pager_backed_cache_policy = std::nullopt;
      break;
    case fuchsia_fs_startup::wire::EvictionPolicyOverride::kNeverEvict:
      options.pager_backed_cache_policy = CachePolicy::NeverEvict;
      break;
    case fuchsia_fs_startup::wire::EvictionPolicyOverride::kEvictImmediately:
      options.pager_backed_cache_policy = CachePolicy::EvictImmediately;
      break;
    default:
      ZX_PANIC("Unknown cache eviction policy override: %d",
               static_cast<uint32_t>(start_options.cache_eviction_policy_override));
  }

  return options;
}

FilesystemOptions ParseFormatOptions(
    const fuchsia_fs_startup::wire::FormatOptions& format_options) {
  FilesystemOptions options;

  if (format_options.has_num_inodes()) {
    options.num_inodes = format_options.num_inodes();
  }
  if (format_options.has_deprecated_padded_blobfs_format() &&
      format_options.deprecated_padded_blobfs_format()) {
    options.blob_layout_format = BlobLayoutFormat::kDeprecatedPaddedMerkleTreeAtStart;
  }

  return options;
}

MountOptions MergeComponentConfigIntoMountOptions(const ComponentOptions& config,
                                                  MountOptions options) {
  options.paging_threads = std::max(1, config.pager_threads);
  return options;
}

}  // namespace

StartupService::StartupService(async_dispatcher_t* dispatcher, const ComponentOptions& config,
                               ConfigureCallback cb)
    : fs::Service([dispatcher, this](fidl::ServerEnd<fuchsia_fs_startup::Startup> server_end) {
        fidl::BindServer(dispatcher, std::move(server_end), this);
        return ZX_OK;
      }),
      component_config_(config),
      configure_(std::move(cb)) {}

void StartupService::Start(StartRequestView request, StartCompleter::Sync& completer) {
  completer.Reply([&]() -> zx::result<> {
    zx::result device = block_client::RemoteBlockDevice::Create(
        fidl::ClientEnd<fuchsia_hardware_block_volume::Volume>(request->device.TakeChannel()));
    if (device.is_error()) {
      FX_PLOGS(ERROR, device.error_value()) << "Could not initialize block device";
      return device.take_error();
    }
    return configure_(std::move(device.value()),
                      MergeComponentConfigIntoMountOptions(component_config_,
                                                           ParseMountOptions(request->options)));
  }());
}

void StartupService::Format(FormatRequestView request, FormatCompleter::Sync& completer) {
  completer.Reply([&]() -> zx::result<> {
    zx::result device = block_client::RemoteBlockDevice::Create(
        fidl::ClientEnd<fuchsia_hardware_block_volume::Volume>(request->device.TakeChannel()));
    if (device.is_error()) {
      FX_PLOGS(ERROR, device.error_value()) << "Could not initialize block device";
      return device.take_error();
    }
    zx_status_t status =
        FormatFilesystem(device.value().get(), ParseFormatOptions(request->options));
    if (status != ZX_OK) {
      FX_PLOGS(ERROR, status) << "Failed to format blobfs";
      return zx::error(status);
    }
    return zx::ok();
  }());
}

void StartupService::Check(CheckRequestView request, CheckCompleter::Sync& completer) {
  completer.Reply([&]() -> zx::result<> {
    zx::result device = block_client::RemoteBlockDevice::Create(
        fidl::ClientEnd<fuchsia_hardware_block_volume::Volume>(request->device.TakeChannel()));
    if (device.is_error()) {
      FX_PLOGS(ERROR, device.error_value()) << "Could not initialize block device";
      return device.take_error();
    }
    // Blobfs supports none of the check options.
    MountOptions options;
    zx_status_t status = Fsck(std::move(device.value()), options);
    if (status != ZX_OK) {
      FX_PLOGS(ERROR, status) << "Consistency check failed for blobfs";
      return zx::error(status);
    }
    return zx::ok();
  }());
}

}  // namespace blobfs
