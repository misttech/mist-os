// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_STORAGE_LIB_PAVER_BLOCK_DEVICES_H_
#define SRC_STORAGE_LIB_PAVER_BLOCK_DEVICES_H_

#include <fidl/fuchsia.device/cpp/wire.h>
#include <fidl/fuchsia.hardware.block.volume/cpp/wire.h>
#include <fidl/fuchsia.io/cpp/wire.h>
#include <fidl/fuchsia.storage.partitions/cpp/markers.h>
#include <lib/fit/function.h>
#include <lib/zx/channel.h>
#include <lib/zx/result.h>

#include <fbl/unique_fd.h>

namespace paver {

// Interface to establish new connections to fuchsia.hardware.block.volume.Volume.
class VolumeConnector {
 public:
  virtual zx::result<fidl::ClientEnd<fuchsia_hardware_block_volume::Volume>> Connect() const = 0;
  // This method will assert if called on a non-PartitionServiceBasedVolumeConnector.
  virtual zx::result<fidl::ClientEnd<fuchsia_storage_partitions::Partition>> PartitionManagement()
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
  zx::result<fidl::ClientEnd<fuchsia_storage_partitions::Partition>> PartitionManagement()
      const override;
  fidl::UnownedClientEnd<fuchsia_device::Controller> Controller() const override;
  fidl::ClientEnd<fuchsia_device::Controller> TakeController() override;

 private:
  fidl::WireSyncClient<fuchsia_device::Controller> controller_;
};

// A VolumeConnector backed by a directory which contains a node to vend connections to
// fuchsia.hardware.block.volume.Volume.
class DirBasedVolumeConnector : public VolumeConnector {
 public:
  DirBasedVolumeConnector(fbl::unique_fd dir, std::string volume_connector_path);

  zx::result<fidl::ClientEnd<fuchsia_hardware_block_volume::Volume>> Connect() const override;
  zx::result<fidl::ClientEnd<fuchsia_storage_partitions::Partition>> PartitionManagement()
      const override;
  fidl::UnownedClientEnd<fuchsia_device::Controller> Controller() const override;
  fidl::ClientEnd<fuchsia_device::Controller> TakeController() override;

 protected:
  fbl::unique_fd dir_;

 private:
  std::string volume_connector_path_;
};

// Like `DirBasedVolumeConnector`, but the service has an additional "partition" node for connecting
// to fuchsia.storage.partitions.Partition instances.
class PartitionServiceBasedVolumeConnector : public DirBasedVolumeConnector {
 public:
  explicit PartitionServiceBasedVolumeConnector(fbl::unique_fd service_dir);

  zx::result<fidl::ClientEnd<fuchsia_storage_partitions::Partition>> PartitionManagement()
      const override;
};

// An abstraction for accessing block devices, either via devfs or
// fuchsia.storage.partitions.PartitionService.
class BlockDevices {
 public:
  // Creates an instance that searches for devices in Devfs.
  // `devfs_root` can be injected for testing.  If unspecified, the devfs root will be connected to
  // /dev.
  static zx::result<BlockDevices> CreateDevfs(fbl::unique_fd devfs_root = {});

  // Creates an instance that searches for devices from fshost's /block.
  // Note that fshost will forward GPT-managed partitions as instances of the PartitionService
  // instead; see CreateFromPartitionService.  Only non-GPT devices will be published here.
  // `block_dir` can be injected for testing.  If unspecified, /block will be opened.
  static zx::result<BlockDevices> CreateFromFshostBlockDir(fbl::unique_fd block_dir = {});

  // Creates an instance that searches for GPT-managed devices as instances of PartitionService.
  // `svc_root` should contain the `fuchsia.storage.partitions.PartitionService` service.
  static zx::result<BlockDevices> CreateFromPartitionService(
      fidl::UnownedClientEnd<fuchsia_io::Directory> svc_root);

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

  const fbl::unique_fd& devfs_root() const {
    ZX_ASSERT(variant_ == Variant::kDevfs);
    return root_;
  }

  bool IsStorageHost() const;

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
  enum Variant : uint8_t {
    kDevfs,
    kFshostBlockDir,
    kPartitionService,
  };

  BlockDevices(fbl::unique_fd root, Variant variant);

  zx::result<std::vector<std::unique_ptr<VolumeConnector>>> OpenAllPartitionsInner(
      fit::function<bool(const zx::channel&)> filter, size_t limit) const;

  fbl::unique_fd root_;
  Variant variant_;
};

}  // namespace paver

#endif  // SRC_STORAGE_LIB_PAVER_BLOCK_DEVICES_H_
