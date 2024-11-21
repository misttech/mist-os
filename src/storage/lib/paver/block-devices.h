// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_STORAGE_LIB_PAVER_BLOCK_DEVICES_H_
#define SRC_STORAGE_LIB_PAVER_BLOCK_DEVICES_H_

#include <fidl/fuchsia.device/cpp/wire.h>
#include <fidl/fuchsia.hardware.block.volume/cpp/wire.h>
#include <fidl/fuchsia.storagehost/cpp/markers.h>
#include <lib/fit/function.h>
#include <lib/zx/channel.h>
#include <lib/zx/result.h>

#include <fbl/unique_fd.h>

namespace paver {

// Interface to establish new connections to fuchsia.hardware.block.volume.Volume.
class VolumeConnector {
 public:
  virtual zx::result<fidl::ClientEnd<fuchsia_hardware_block_volume::Volume>> Connect() const = 0;
  // This method will assert if called on a non-PathBasedVolumeConnector.
  virtual zx::result<fidl::ClientEnd<fuchsia_storagehost::Partition>> PartitionManagement()
      const = 0;
  // The following two methods will assert if called on a non-DevfsVolumeConnector.
  // TODO(https://fxbug.dev/339491886): Remove once remaining use-cases are ported to storage-host.
  virtual fidl::UnownedClientEnd<fuchsia_device::Controller> Controller() const = 0;
  virtual fidl::ClientEnd<fuchsia_device::Controller> TakeController() = 0;

  virtual ~VolumeConnector() = default;
};

// A VolumeConnector backed by devfs.
class DevfsVolumeConnector : public VolumeConnector {
 public:
  explicit DevfsVolumeConnector(fidl::ClientEnd<fuchsia_device::Controller>);

  zx::result<fidl::ClientEnd<fuchsia_hardware_block_volume::Volume>> Connect() const override;
  zx::result<fidl::ClientEnd<fuchsia_storagehost::Partition>> PartitionManagement() const override;
  fidl::UnownedClientEnd<fuchsia_device::Controller> Controller() const override;
  fidl::ClientEnd<fuchsia_device::Controller> TakeController() override;

 private:
  fidl::WireSyncClient<fuchsia_device::Controller> controller_;
};

// A VolumeConnector backed by a service node relative to a directory (see fdio_service_connect_at).
// It's expected that the service node at `path` has two nodes, "volume" and "partition".  See
// fuchsia.storagehost.PartitionService.
class ServiceBasedVolumeConnector : public VolumeConnector {
 public:
  explicit ServiceBasedVolumeConnector(fbl::unique_fd service_dir);

  zx::result<fidl::ClientEnd<fuchsia_hardware_block_volume::Volume>> Connect() const override;
  zx::result<fidl::ClientEnd<fuchsia_storagehost::Partition>> PartitionManagement() const override;
  fidl::UnownedClientEnd<fuchsia_device::Controller> Controller() const override;
  fidl::ClientEnd<fuchsia_device::Controller> TakeController() override;

 private:
  fbl::unique_fd service_dir_;
};

// An abstraction for accessing block devices, either via devfs or the partitions directory
// exposed by storage-host.
// The partitions directory will be preferred (i.e. on systems which use storage-host).
class BlockDevices {
 public:
  // `devfs_root` and `partitions_root` can be injected for testing.  If unspecified, the devfs root
  // will be connected to /dev, and the partitions root will be connected to /partitions (if
  // available).
  static zx::result<BlockDevices> Create(fbl::unique_fd devfs_root = {},
                                         fbl::unique_fd partitions_root = {});

  // Creates an empty BlockDevices instance which never yields any partitions.  Useful for tests
  // which need a valid instance but don't actually use it.
  static BlockDevices CreateEmpty();

  BlockDevices(BlockDevices&&) = default;
  BlockDevices& operator=(BlockDevices&&) = default;

  BlockDevices(const BlockDevices&) = delete;
  BlockDevices& operator=(const BlockDevices&) = delete;

  // Duplicates the underlying connection to create a new BlockDevices.
  // Separate from the copy constructors (which are deleted) to avoid unintentional expensive
  // operations.
  BlockDevices Duplicate() const;

  const fbl::unique_fd& devfs_root() const { return devfs_root_; }

  bool HasPartitionsDirectory() const;

  // Returns a connector for every partition that matches the filter.  A channel connected to the
  // fuchsia.hardware.block.partition.Partition service of the block device is provided to `filter`.
  zx::result<std::vector<std::unique_ptr<VolumeConnector>>> OpenAllPartitions(
      fit::function<bool(const zx::channel&)> filter) const;

  // Returns a connector for the first partition that matches the filter.  A channel connected to
  // the fuchsia.hardware.block.partition.Partition service of the block device is provided to
  // `filter`.
  zx::result<std::unique_ptr<VolumeConnector>> OpenPartition(
      fit::function<bool(const zx::channel&)> filter) const;

  // Like `OpenPartition`, but will wait for newly added partitions (at least until `timeout`).
  zx::result<std::unique_ptr<VolumeConnector>> WaitForPartition(
      fit::function<bool(const zx::channel&)> filter, zx_duration_t timeout,
      // TODO(https://fxbug.dev/339491886): Support skip-block and remove `devfs_suffix`
      const char* devfs_suffix = "class/block") const;

 private:
  BlockDevices(fbl::unique_fd devfs_root, fbl::unique_fd partitions_root);

  zx::result<std::vector<std::unique_ptr<VolumeConnector>>> OpenAllPartitionsInner(
      fit::function<bool(const zx::channel&)> filter, size_t limit) const;

  fbl::unique_fd devfs_root_;
  fbl::unique_fd partitions_root_;
};

}  // namespace paver

#endif  // SRC_STORAGE_LIB_PAVER_BLOCK_DEVICES_H_
