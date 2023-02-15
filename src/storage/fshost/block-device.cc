// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "block-device.h"

#include <dirent.h>
#include <fcntl.h>
#include <fidl/fuchsia.device.manager/cpp/markers.h>
#include <fidl/fuchsia.device/cpp/wire.h>
#include <fidl/fuchsia.fs.startup/cpp/wire.h>
#include <fidl/fuchsia.fs/cpp/wire.h>
#include <fidl/fuchsia.hardware.block.volume/cpp/wire.h>
#include <fidl/fuchsia.io/cpp/wire_types.h>
#include <inttypes.h>
#include <lib/component/incoming/cpp/clone.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/fdio/cpp/caller.h>
#include <lib/fdio/directory.h>
#include <lib/fdio/fd.h>
#include <lib/fdio/fdio.h>
#include <lib/fdio/unsafe.h>
#include <lib/fdio/watcher.h>
#include <lib/fzl/time.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/channel.h>
#include <lib/zx/result.h>
#include <lib/zx/time.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>
#include <zircon/device/block.h>
#include <zircon/errors.h>
#include <zircon/processargs.h>
#include <zircon/status.h>
#include <zircon/syscalls.h>
#include <zircon/types.h>

#include <cctype>
#include <memory>
#include <utility>

#include <fbl/algorithm.h>
#include <fbl/string_buffer.h>
#include <fbl/unique_fd.h>
#include <fbl/vector.h>
#include <gpt/gpt.h>
#include <gpt/guid.h>

#include "constants.h"
#include "encrypted-volume.h"
#include "src/devices/block/drivers/block-verity/verified-volume-client.h"
#include "src/lib/storage/block_client/cpp/remote_block_device.h"
#include "src/lib/storage/fs_management/cpp/admin.h"
#include "src/lib/storage/fs_management/cpp/format.h"
#include "src/lib/storage/fs_management/cpp/mount.h"
#include "src/lib/storage/fs_management/cpp/options.h"
#include "src/lib/uuid/uuid.h"
#include "src/storage/fshost/block-device-interface.h"
#include "src/storage/fshost/constants.h"
#include "src/storage/fshost/fxfs.h"
#include "src/storage/fshost/utils.h"
#include "src/storage/minfs/fsck.h"
#include "src/storage/minfs/minfs.h"

namespace fshost {
namespace {

using fs_management::DiskFormat;

const char kAllowAuthoringFactoryConfigFile[] = "/boot/config/allow-authoring-factory";

// Runs the binary indicated in `argv`, which must always be terminated with nullptr.
// `device_channel`, containing a handle to the block device, is passed to the binary.  If
// `export_root` is specified, the binary is launched asynchronously.  Otherwise, this waits for the
// binary to terminate and returns the status.
zx_status_t RunBinary(const fbl::Vector<const char*>& argv,
                      fidl::ClientEnd<fuchsia_io::Node> device,
                      fidl::ServerEnd<fuchsia_io::Directory> export_root = {}) {
  FX_CHECK(argv[argv.size() - 1] == nullptr);
  zx::process proc;
  int handle_count = 1;
  zx_handle_t handles[2] = {device.TakeChannel().release()};
  uint32_t handle_ids[2] = {FS_HANDLE_BLOCK_DEVICE_ID};
  bool async = false;
  if (export_root) {
    handles[handle_count] = export_root.TakeChannel().release();
    handle_ids[handle_count] = PA_DIRECTORY_REQUEST;
    ++handle_count;
    async = true;
  }
  if (zx_status_t status = Launch(*zx::job::default_job(), argv[0], argv.data(), nullptr, -1,
                                  /* TODO(fxbug.dev/32044) */ zx::resource(), handles, handle_ids,
                                  handle_count, &proc);
      status != ZX_OK) {
    FX_LOGS(ERROR) << "Failed to launch binary: " << argv[0];
    return status;
  }

  if (async)
    return ZX_OK;

  if (zx_status_t status = proc.wait_one(ZX_PROCESS_TERMINATED, zx::time::infinite(), nullptr);
      status != ZX_OK) {
    FX_LOGS(ERROR) << "Error waiting for process to terminate";
    return status;
  }

  zx_info_process_t info;
  if (zx_status_t status = proc.get_info(ZX_INFO_PROCESS, &info, sizeof(info), nullptr, nullptr);
      status != ZX_OK) {
    FX_LOGS(ERROR) << "Failed to get process info";
    return status;
  }

  if (!(info.flags & ZX_INFO_PROCESS_FLAG_EXITED) || info.return_code != 0) {
    FX_LOGS(ERROR) << "flags: " << info.flags << ", return_code: " << info.return_code;
    return ZX_ERR_BAD_STATE;
  }

  return ZX_OK;
}

Copier TryReadingFilesystem(fidl::ClientEnd<fuchsia_io::Directory> export_root) {
  auto root_dir_or = fs_management::FsRootHandle(export_root);
  if (root_dir_or.is_error())
    return {};

  fbl::unique_fd fd;
  if (zx_status_t status =
          fdio_fd_create(root_dir_or->TakeChannel().release(), fd.reset_and_get_address());
      status != ZX_OK) {
    FX_LOGS(ERROR) << "fdio_fd_create failed: " << zx_status_get_string(status);
    return {};
  }

  // Clone the handle so that we can unmount.
  zx::channel root_dir_handle;
  if (zx_status_t status = fdio_fd_clone(fd.get(), root_dir_handle.reset_and_get_address());
      status != ZX_OK) {
    FX_LOGS(ERROR) << "fdio_fd_clone failed: " << zx_status_get_string(status);
    return {};
  }

  fidl::ClientEnd<fuchsia_io::Directory> root_dir_client(std::move(root_dir_handle));
  auto unmount = fit::defer([&export_root] {
    auto admin_client = component::ConnectAt<fuchsia_fs::Admin>(export_root);
    if (admin_client.is_ok()) {
      [[maybe_unused]] auto result = fidl::WireCall(*admin_client)->Shutdown();
    }
  });

  auto copier_or = Copier::Read(std::move(fd));
  if (copier_or.is_error()) {
    FX_LOGS(ERROR) << "Copier::Read: " << copier_or.status_string();
    return {};
  }
  return std::move(copier_or).value();
}

// Tries to mount Minfs and reads all data found on the minfs partition.  Errors are ignored.
Copier TryReadingMinfs(fidl::ClientEnd<fuchsia_io::Node> device) {
  fbl::Vector<const char*> argv = {kMinfsPath, "mount", nullptr};
  auto export_root_or = fidl::CreateEndpoints<fuchsia_io::Directory>();
  if (export_root_or.is_error())
    return {};
  if (RunBinary(argv, std::move(device), std::move(export_root_or->server)) != ZX_OK)
    return {};
  return TryReadingFilesystem(std::move(export_root_or->client));
}

}  // namespace

zx::result<std::string> GetTopologicalPath(
    fidl::UnownedClientEnd<fuchsia_device::Controller> controller) {
  const fidl::WireResult result = fidl::WireCall(controller)->GetTopologicalPath();
  if (!result.ok()) {
    return zx::error(result.status());
  }
  fit::result response = result.value();
  if (response.is_error()) {
    return response.take_error();
  }
  return zx::ok(std::string{response.value()->path.get()});
}

fs_management::MountOptions GetBlobfsMountOptions(const fshost_config::Config& config,
                                                  const FshostBootArgs* boot_args) {
  fs_management::MountOptions options;
  options.component_child_name = "blobfs";
  options.write_compression_level = -1;
  options.sandbox_decompression = config.sandbox_decompression();
  if (boot_args) {
    if (boot_args->blobfs_write_compression_algorithm()) {
      // Ignore invalid options.
      if (boot_args->blobfs_write_compression_algorithm() == "ZSTD_CHUNKED" ||
          boot_args->blobfs_write_compression_algorithm() == "UNCOMPRESSED") {
        options.write_compression_algorithm = boot_args->blobfs_write_compression_algorithm();
      }
    }
    if (boot_args->blobfs_eviction_policy()) {
      // Ignore invalid options.
      if (boot_args->blobfs_eviction_policy() == "NEVER_EVICT" ||
          boot_args->blobfs_eviction_policy() == "EVICT_IMMEDIATELY") {
        options.cache_eviction_policy = boot_args->blobfs_eviction_policy();
      }
    }
  }
  return options;
}

fs_management::MountOptions GetBlobfsMountOptionsForRecovery(const fshost_config::Config& config) {
  return {
      .readonly = false,
      .wait_until_ready = true,
      .write_compression_level = -1,
      .sandbox_decompression = config.sandbox_decompression(),
      .component_child_name = "blobfs",
      .component_collection_name = "fs-collection",
  };
}

BlockDevice::BlockDevice(FilesystemMounter* mounter,
                         fidl::ClientEnd<fuchsia_hardware_block::Block> block,
                         const fshost_config::Config* device_config)
    : mounter_(mounter),
      device_config_(device_config),
      block_(std::move(block)),
      content_format_(fs_management::kDiskFormatUnknown),
      topological_path_([this]() {
        zx::result topological_path = GetTopologicalPath(
            fidl::UnownedClientEnd<fuchsia_device::Controller>(block_.channel().borrow()));
        if (topological_path.is_error()) {
          FX_PLOGS(WARNING, topological_path.error_value()) << "Unable to get topological path";
        }
        return topological_path.value_or(std::string{});
      }()) {}

zx::result<std::unique_ptr<BlockDeviceInterface>> BlockDevice::OpenBlockDevice(
    const char* topological_path) const {
  zx::result device = component::Connect<fuchsia_hardware_block::Block>(topological_path);
  if (device.is_error()) {
    FX_PLOGS(WARNING, device.error_value()) << "Failed to open block device " << topological_path;
    return device.take_error();
  }
  return zx::ok(std::make_unique<BlockDevice>(mounter_, std::move(device.value()), device_config_));
}

zx::result<std::unique_ptr<BlockDeviceInterface>> BlockDevice::OpenBlockDeviceByFd(
    fbl::unique_fd fd) const {
  zx::result device = fdio_cpp::FdioCaller(std::move(fd)).take_as<fuchsia_hardware_block::Block>();
  if (device.is_error()) {
    FX_PLOGS(WARNING, device.error_value()) << "Failed to acquire block channel";
    return device.take_error();
  }
  return zx::ok(std::make_unique<BlockDevice>(mounter_, std::move(device.value()), device_config_));
}

void BlockDevice::AddData(Copier copier) { source_data_ = std::move(copier); }

zx::result<Copier> BlockDevice::ExtractData() {
  if (content_format() != fs_management::kDiskFormatMinfs) {
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }
  zx::result cloned = component::Clone(block_, component::AssumeProtocolComposesNode);
  if (cloned.is_error()) {
    return cloned.take_error();
  }
  fidl::ClientEnd<fuchsia_io::Node> node(cloned.value().TakeChannel());
  return zx::ok(TryReadingMinfs(std::move(node)));
}

DiskFormat BlockDevice::content_format() const {
  if (content_format_ != fs_management::kDiskFormatUnknown) {
    return content_format_;
  }
  content_format_ = fs_management::DetectDiskFormat(block_);
  return content_format_;
}

DiskFormat BlockDevice::GetFormat() { return format_; }

void BlockDevice::SetFormat(DiskFormat format) { format_ = format; }

const std::string& BlockDevice::partition_name() const {
  if (!partition_name_.empty()) {
    return partition_name_;
  }
  // The block device might not support the partition protocol in which case the connection will
  // be closed, so clone the channel in case that happens.
  //
  // TODO(https://fxbug.dev/112484): this relies on multiplexing.
  //
  // TODO(https://fxbug.dev/113512): Remove this.
  zx::result channel =
      component::Clone(fidl::UnownedClientEnd<fuchsia_hardware_block_partition::Partition>(
                           block_.channel().borrow()),
                       component::AssumeProtocolComposesNode);
  if (channel.is_error()) {
    FX_PLOGS(ERROR, channel.status_value()) << "Unable to clone partition channel";
    return partition_name_;
  }
  const fidl::WireResult result = fidl::WireCall(channel.value())->GetName();
  if (!result.ok()) {
    FX_LOGS(ERROR) << "Unable to get partition name (fidl error): " << result.FormatDescription();
    return partition_name_;
  }
  const fidl::WireResponse response = result.value();
  if (zx_status_t status = response.status; status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "Unable to get partition name";
    return partition_name_;
  }
  partition_name_ = response.name.get();
  return partition_name_;
}

zx::result<fuchsia_hardware_block::wire::BlockInfo> BlockDevice::GetInfo() const {
  if (info_.has_value()) {
    return zx::ok(*info_);
  }
  const fidl::WireResult result = fidl::WireCall(block_)->GetInfo();
  if (!result.ok()) {
    return zx::error(result.status());
  }
  fit::result response = result.value();
  if (response.is_error()) {
    return response.take_error();
  }
  return zx::ok(info_.emplace(response->info));
}

const fuchsia_hardware_block_partition::wire::Guid& BlockDevice::GetInstanceGuid() const {
  if (instance_guid_.has_value()) {
    return instance_guid_.value();
  }
  fuchsia_hardware_block_partition::wire::Guid& guid = instance_guid_.emplace();
  // The block device might not support the partition protocol in which case the connection will
  // be closed, so clone the channel in case that happens.
  //
  // TODO(https://fxbug.dev/112484): this relies on multiplexing.
  //
  // TODO(https://fxbug.dev/113512): Remove this.
  zx::result channel =
      component::Clone(fidl::UnownedClientEnd<fuchsia_hardware_block_partition::Partition>(
                           block_.channel().borrow()),
                       component::AssumeProtocolComposesNode);
  if (channel.is_error()) {
    FX_PLOGS(ERROR, channel.status_value()) << "Unable to clone partition channel";
    return guid;
  }
  const fidl::WireResult result = fidl::WireCall(channel.value())->GetInstanceGuid();
  if (!result.ok()) {
    FX_LOGS(ERROR) << "Unable to get partition instance GUID (fidl error): "
                   << result.FormatDescription();
    return guid;
  }
  const fidl::WireResponse response = result.value();
  if (zx_status_t status = response.status; status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "Unable to get partition instance GUID";
    return guid;
  }
  guid = *response.guid;
  return guid;
}

const fuchsia_hardware_block_partition::wire::Guid& BlockDevice::GetTypeGuid() const {
  if (type_guid_.has_value()) {
    return type_guid_.value();
  }
  fuchsia_hardware_block_partition::wire::Guid& guid = type_guid_.emplace();
  // The block device might not support the partition protocol in which case the connection will
  // be closed, so clone the channel in case that happens.
  //
  // TODO(https://fxbug.dev/112484): this relies on multiplexing.
  //
  // TODO(https://fxbug.dev/113512): Remove this.
  zx::result channel =
      component::Clone(fidl::UnownedClientEnd<fuchsia_hardware_block_partition::Partition>(
                           block_.channel().borrow()),
                       component::AssumeProtocolComposesNode);
  if (channel.is_error()) {
    FX_PLOGS(ERROR, channel.status_value()) << "Unable to clone partition channel";
    return guid;
  }
  const fidl::WireResult result = fidl::WireCall(channel.value())->GetTypeGuid();
  if (!result.ok()) {
    FX_LOGS(ERROR) << "Unable to get partition type GUID (fidl error): "
                   << result.FormatDescription();
    return guid;
  }
  const fidl::WireResponse response = result.value();
  if (zx_status_t status = response.status; status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "Unable to get partition type GUID";
    return guid;
  }
  guid = *response.guid;
  return guid;
}

zx_status_t BlockDevice::AttachDriver(const std::string_view& driver) {
  FX_LOGS(INFO) << "Binding: " << driver;
  const fidl::WireResult result =
      fidl::WireCall(fidl::UnownedClientEnd<fuchsia_device::Controller>(block_.channel().borrow()))
          ->Bind(fidl::StringView::FromExternal(driver));
  if (!result.ok()) {
    FX_PLOGS(ERROR, result.status()) << "Failed to attach driver: " << driver;
    return result.status();
  }
  const fit::result response = result.value();
  if (response.is_error()) {
    FX_PLOGS(ERROR, response.error_value()) << "Failed to attach driver: " << driver;
    return response.error_value();
  }
  return ZX_OK;
}

zx_status_t BlockDevice::UnsealZxcrypt() {
  FX_LOGS(INFO) << "unsealing zxcrypt with UUID "
                << uuid::Uuid(GetInstanceGuid().value.data()).ToString();
  // Bind and unseal the driver from a separate thread, since we
  // have to wait for a number of devices to do I/O and settle,
  // and we don't want to block block-watcher for any nontrivial
  // length of time.

  zx::result cloned = component::Clone(block_, component::AssumeProtocolComposesNode);
  if (cloned.is_error()) {
    return cloned.error_value();
  }
  fbl::unique_fd fd;
  if (zx_status_t status =
          fdio_fd_create(cloned.value().TakeChannel().release(), fd.reset_and_get_address());
      status != ZX_OK) {
    return status;
  }

  std::thread thread(
      [volume = EncryptedVolume(std::move(fd), fbl::unique_fd(open("/dev", O_RDONLY)))]() mutable {
        volume.EnsureUnsealedAndFormatIfNeeded();
      });
  thread.detach();

  return ZX_OK;
}

zx_status_t BlockDevice::OpenBlockVerityForVerifiedRead(std::string seal_hex) {
  FX_LOGS(INFO) << "preparing block-verity";

  digest::Digest seal;
  if (zx_status_t status = seal.Parse(seal_hex.c_str()); status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "block-verity seal " << seal_hex
                            << " did not parse as SHA256 hex digest";
    return status;
  }

  std::thread thread([controller = fidl::UnownedClientEnd<fuchsia_device::Controller>(
                          block_.channel().borrow()),
                      seal = std::move(seal)]() {
    fbl::unique_fd devfs_root(open("/dev", O_RDONLY));

    std::unique_ptr<block_verity::VerifiedVolumeClient> vvc;
    if (zx_status_t status = block_verity::VerifiedVolumeClient::CreateFromBlockDevice(
            controller, std::move(devfs_root),
            block_verity::VerifiedVolumeClient::Disposition::kDriverAlreadyBound, zx::sec(5), &vvc);
        status != ZX_OK) {
      FX_PLOGS(ERROR, status) << "Couldn't create VerifiedVolumeClient";
      return;
    }

    fbl::unique_fd inner_block_fd;
    if (zx_status_t status = vvc->OpenForVerifiedRead(seal, zx::sec(5), inner_block_fd);
        status != ZX_OK) {
      FX_PLOGS(ERROR, status) << "OpenForVerifiedRead failed";
    }
  });
  thread.detach();

  return ZX_OK;
}

zx_status_t BlockDevice::FormatZxcrypt() {
  fbl::unique_fd devfs_root_fd(open("/dev", O_RDONLY));
  if (!devfs_root_fd) {
    return ZX_ERR_NOT_FOUND;
  }
  zx::result cloned = component::Clone(block_, component::AssumeProtocolComposesNode);
  if (cloned.is_error()) {
    return cloned.error_value();
  }
  fbl::unique_fd fd;
  if (zx_status_t status =
          fdio_fd_create(cloned.value().TakeChannel().release(), fd.reset_and_get_address());
      status != ZX_OK) {
    return status;
  }

  EncryptedVolume volume(std::move(fd), std::move(devfs_root_fd));
  return volume.Format();
}

zx::result<std::string> BlockDevice::VeritySeal() {
  return mounter_->boot_args()->block_verity_seal();
}

bool BlockDevice::ShouldAllowAuthoringFactory() {
  // Checks for presence of /boot/config/allow-authoring-factory
  fbl::unique_fd allow_authoring_factory_fd(open(kAllowAuthoringFactoryConfigFile, O_RDONLY));
  return allow_authoring_factory_fd.is_valid();
}

bool BlockDevice::IsRamDisk() const {
  auto ramdisk_prefix = device_config_->ramdisk_prefix();
  ZX_DEBUG_ASSERT(!ramdisk_prefix.empty());
  return topological_path().compare(0, ramdisk_prefix.length(), ramdisk_prefix) == 0;
}

zx_status_t BlockDevice::SetPartitionMaxSize(const std::string& fvm_path, uint64_t max_byte_size) {
  // Get the partition GUID for talking to FVM.
  const fuchsia_hardware_block_partition::wire::Guid& instance_guid = GetInstanceGuid();
  if (std::all_of(std::begin(instance_guid.value), std::end(instance_guid.value),
                  [](auto val) { return val == 0; }))
    return ZX_ERR_NOT_SUPPORTED;  // Not a partition, nothing to do.

  fbl::unique_fd fvm_fd(open(fvm_path.c_str(), O_RDONLY));
  if (!fvm_fd)
    return ZX_ERR_NOT_SUPPORTED;  // Not in FVM, nothing to do.
  fdio_cpp::UnownedFdioCaller fvm_caller(fvm_fd);

  // Get the FVM slice size.
  auto info_response =
      fidl::WireCall(fidl::UnownedClientEnd<fuchsia_hardware_block_volume::VolumeManager>(
                         fvm_caller.borrow_channel()))
          ->GetInfo();
  if (info_response.status() != ZX_OK) {
    FX_LOGS(ERROR) << "Unable to request FVM Info: "
                   << zx_status_get_string(info_response.status());
    return info_response.status();
  }
  if (info_response.value().status != ZX_OK || !info_response.value().info) {
    FX_LOGS(ERROR) << "FVM info request failed: "
                   << zx_status_get_string(info_response.value().status);
    return info_response.value().status;
  }
  uint64_t slice_size = info_response.value().info->slice_size;

  // Set the limit (convert to slice units, rounding down).
  uint64_t max_slice_count = max_byte_size / slice_size;
  auto response =
      fidl::WireCall(fidl::UnownedClientEnd<fuchsia_hardware_block_volume::VolumeManager>(
                         fvm_caller.borrow_channel()))
          ->SetPartitionLimit(instance_guid, max_slice_count);
  if (response.status() != ZX_OK || response.value().status != ZX_OK) {
    FX_LOGS(ERROR) << "Unable to set partition limit for " << topological_path() << " to "
                   << max_byte_size << " bytes (" << max_slice_count << " slices).";
    if (response.status() != ZX_OK) {
      FX_LOGS(ERROR) << "  FIDL error: " << zx_status_get_string(response.status());
      return response.status();
    }
    FX_LOGS(ERROR) << " FVM error: " << zx_status_get_string(response.value().status);
    return response.value().status;
  }

  return ZX_OK;
}

zx_status_t BlockDevice::SetPartitionName(const std::string& fvm_path, std::string_view name) {
  // Get the partition GUID for talking to FVM.
  const fuchsia_hardware_block_partition::wire::Guid& instance_guid = GetInstanceGuid();
  if (std::all_of(std::begin(instance_guid.value), std::end(instance_guid.value),
                  [](auto val) { return val == 0; }))
    return ZX_ERR_NOT_SUPPORTED;  // Not a partition, nothing to do.

  fbl::unique_fd fvm_fd(open(fvm_path.c_str(), O_RDONLY));
  if (!fvm_fd)
    return ZX_ERR_NOT_SUPPORTED;  // Not in FVM, nothing to do.

  // Actually set the name.
  fdio_cpp::UnownedFdioCaller caller(fvm_fd);
  auto response =
      fidl::WireCall(fidl::UnownedClientEnd<fuchsia_hardware_block_volume::VolumeManager>(
                         caller.borrow_channel()))
          ->SetPartitionName(instance_guid, fidl::StringView::FromExternal(name));
  if (response.status() != ZX_OK || response->is_error()) {
    FX_LOGS(ERROR) << "Unable to set partition name for " << topological_path() << " to '" << name
                   << "'.";
    if (response.status() != ZX_OK) {
      FX_LOGS(ERROR) << "  FIDL error: " << zx_status_get_string(response.status());
      return response.status();
    }
    FX_LOGS(ERROR) << " FVM error: " << zx_status_get_string(response->error_value());
    return response->error_value();
  }

  return ZX_OK;
}

bool BlockDevice::ShouldCheckFilesystems() { return mounter_->ShouldCheckFilesystems(); }

zx_status_t BlockDevice::CheckFilesystem() {
  if (!ShouldCheckFilesystems()) {
    return ZX_OK;
  }

  zx::result info = GetInfo();
  if (info.is_error()) {
    return info.status_value();
  }

  const std::array<DiskFormat, 3> kFormatsToCheck = {
      fs_management::kDiskFormatMinfs,
      fs_management::kDiskFormatF2fs,
      fs_management::kDiskFormatFxfs,
  };
  if (std::find(kFormatsToCheck.begin(), kFormatsToCheck.end(), format_) == kFormatsToCheck.end()) {
    FX_LOGS(INFO) << "Skipping consistency checker for partition of type "
                  << DiskFormatString(format_);
    return ZX_OK;
  }

  zx::ticks before = zx::ticks::now();
  auto timer = fit::defer([before]() {
    auto after = zx::ticks::now();
    auto duration = fzl::TicksToNs(after - before);
    FX_LOGS(INFO) << "fsck took " << duration.to_secs() << "." << duration.to_msecs() % 1000
                  << " seconds";
  });
  FX_LOGS(INFO) << "fsck of " << DiskFormatString(format_) << " partition started";

  zx_status_t status;
  switch (format_) {
    case fs_management::kDiskFormatF2fs:
    case fs_management::kDiskFormatFxfs: {
      status = CheckCustomFilesystem(format_);
      break;
    }
    case fs_management::kDiskFormatMinfs: {
      // With minfs, we can run the library directly without needing to start a new process.
      uint64_t device_size = info->block_size * info->block_count / minfs::kMinfsBlockSize;
      zx::result cloned = component::Clone(
          fidl::UnownedClientEnd<fuchsia_hardware_block_volume::Volume>(block_.channel().borrow()),
          component::AssumeProtocolComposesNode);
      if (cloned.is_error()) {
        FX_PLOGS(ERROR, cloned.error_value()) << "Cannot clone block channel";
        return cloned.error_value();
      }
      zx::result device = block_client::RemoteBlockDevice::Create(std::move(cloned.value()));
      if (device.is_error()) {
        FX_PLOGS(ERROR, device.error_value()) << "Cannot create block device";
        return device.error_value();
      }
      zx::result bc =
          minfs::Bcache::Create(std::move(device.value()), static_cast<uint32_t>(device_size));
      if (bc.is_error()) {
        FX_PLOGS(ERROR, bc.error_value()) << "Could not initialize minfs bcache.";
        return bc.error_value();
      }
      status =
          minfs::Fsck(std::move(bc.value()), minfs::FsckOptions{.repair = true}).status_value();
      break;
    }
    default:
      __builtin_unreachable();
  }
  if (status != ZX_OK) {
    FX_LOGS(ERROR) << "\n--------------------------------------------------------------\n"
                      "|\n"
                      "|   WARNING: fshost fsck failure!\n"
                      "|   Corrupt "
                   << DiskFormatString(format_)
                   << " filesystem\n"
                      "|\n"
                      "|   Please file a bug to the Storage component in http://fxbug.dev,\n"
                      "|   including a device snapshot collected with `ffx target snapshot` if\n"
                      "|   possible.\n"
                      "|\n"
                      "--------------------------------------------------------------";
    mounter_->ReportPartitionCorrupted(format_);
  } else {
    FX_LOGS(INFO) << "fsck of " << DiskFormatString(format_) << " completed OK";
  }
  return status;
}

zx_status_t BlockDevice::FormatFilesystem() {
  zx::result info = GetInfo();
  if (info.is_error()) {
    return info.status_value();
  }

  // There might be a previously cached content format; forget that now since it could change.
  content_format_ = fs_management::kDiskFormatUnknown;

  switch (format_) {
    case fs_management::kDiskFormatBlobfs: {
      FX_LOGS(ERROR) << "Not formatting blobfs.";
      return ZX_ERR_NOT_SUPPORTED;
    }
    case fs_management::kDiskFormatFactoryfs: {
      FX_LOGS(ERROR) << "Not formatting factoryfs.";
      return ZX_ERR_NOT_SUPPORTED;
    }
    case fs_management::kDiskFormatFxfs:
    case fs_management::kDiskFormatF2fs: {
      zx_status_t status = FormatCustomFilesystem(format_);
      if (status != ZX_OK) {
        FX_LOGS(ERROR) << "Failed to format: " << zx_status_get_string(status);
      }
      return status;
    }
    case fs_management::kDiskFormatMinfs: {
      // With minfs, we can run the library directly without needing to start a new process.
      FX_LOGS(INFO) << "Formatting minfs.";
      uint64_t blocks = info->block_size * info->block_count / minfs::kMinfsBlockSize;
      zx::result cloned = component::Clone(
          fidl::UnownedClientEnd<fuchsia_hardware_block_volume::Volume>(block_.channel().borrow()),
          component::AssumeProtocolComposesNode);
      if (cloned.is_error()) {
        return cloned.error_value();
      }
      zx::result device = block_client::RemoteBlockDevice::Create(std::move(cloned.value()));
      if (device.is_error()) {
        FX_PLOGS(ERROR, device.error_value()) << "Cannot clone block channel";
        return device.error_value();
      }
      zx::result bc =
          minfs::Bcache::Create(std::move(device.value()), static_cast<uint32_t>(blocks));
      if (bc.is_error()) {
        FX_PLOGS(ERROR, bc.error_value()) << "Could not initialize minfs bcache.";
        return bc.error_value();
      }
      minfs::MountOptions options = {};
      if (zx_status_t status = minfs::Mkfs(options, bc.value().get()).status_value();
          status != ZX_OK) {
        FX_PLOGS(ERROR, status) << "Could not format minfs filesystem.";
        return status;
      }
      FX_LOGS(INFO) << "Minfs filesystem re-formatted. Expect data loss.";
      return ZX_OK;
    }
    default:
      FX_LOGS(ERROR) << "Not formatting unknown filesystem.";
      return ZX_ERR_NOT_SUPPORTED;
  }
}

zx_status_t BlockDevice::MountFilesystem() {
  // TODO(https://fxbug.dev/112484): this relies on multiplexing.
  zx::result block_device = component::Clone(block_, component::AssumeProtocolComposesNode);
  if (block_device.is_error()) {
    return block_device.status_value();
  }
  switch (format_) {
    case fs_management::kDiskFormatFactoryfs: {
      FX_LOGS(INFO) << "BlockDevice::MountFilesystem(factoryfs)";
      fs_management::MountOptions options;
      options.readonly = true;

      zx_status_t status = mounter_->MountFactoryFs(std::move(block_device.value()), options);
      if (status != ZX_OK) {
        FX_LOGS(ERROR) << "Failed to mount factoryfs partition: " << zx_status_get_string(status)
                       << ".";
      }
      return status;
    }
    case fs_management::kDiskFormatBlobfs: {
      FX_LOGS(INFO) << "BlockDevice::MountFilesystem(blobfs)";
      if (zx_status_t status = mounter_->MountBlob(
              std::move(block_device.value()),
              GetBlobfsMountOptions(*device_config_, mounter_->boot_args().get()));
          status != ZX_OK) {
        FX_PLOGS(ERROR, status) << "Failed to mount blobfs partition";
        return status;
      }
      return ZX_OK;
    }
    case fs_management::kDiskFormatFxfs:
    case fs_management::kDiskFormatF2fs:
    case fs_management::kDiskFormatMinfs: {
      fs_management::MountOptions options;

      std::optional<Copier> copier = std::move(source_data_);
      source_data_.reset();

      FX_LOGS(INFO) << "BlockDevice::MountFilesystem(data partition)";
      if (zx_status_t status =
              MountData(options, std::move(copier), std::move(block_device.value()));
          status != ZX_OK) {
        FX_LOGS(ERROR) << "Failed to mount data partition: " << zx_status_get_string(status) << ".";
        return status;
      }
      return ZX_OK;
    }
    default:
      FX_LOGS(ERROR) << "BlockDevice::MountFilesystem(unknown)";
      return ZX_ERR_NOT_SUPPORTED;
  }
}

// Attempt to mount the device at a known location.
//
// If |copier| is set, the data will be copied into the data filesystem before exposing the
// filesystem to clients.  This is only supported for the data guid (i.e. not the durable guid).
//
// Returns ZX_ERR_ALREADY_BOUND if the device could be mounted, but something
// is already mounted at that location. Returns ZX_ERR_INVALID_ARGS if the
// GUID of the device does not match a known valid one. Returns
// ZX_ERR_NOT_SUPPORTED if the GUID is a system GUID. Returns ZX_OK if an
// attempt to mount is made, without checking mount success.
zx_status_t BlockDevice::MountData(const fs_management::MountOptions& options,
                                   std::optional<Copier> copier,
                                   fidl::ClientEnd<fuchsia_hardware_block::Block> block_device) {
  const uint8_t* guid = GetTypeGuid().value.data();
  FX_LOGS(INFO) << "Detected type GUID " << gpt::KnownGuid::TypeDescription(guid)
                << " for data partition";

  if (gpt_is_sys_guid(guid, GPT_GUID_LEN)) {
    return ZX_ERR_NOT_SUPPORTED;
  }
  if (gpt_is_data_guid(guid, GPT_GUID_LEN)) {
    return mounter_->MountData(std::move(block_device), std::move(copier), options, format_);
  }
  FX_LOGS(ERROR) << "Unrecognized type GUID for data partition; not mounting";
  return ZX_ERR_WRONG_TYPE;
}

zx_status_t BlockDeviceInterface::Add(bool format_on_corruption) {
  switch (GetFormat()) {
    case fs_management::kDiskFormatNandBroker: {
      return AttachDriver(kNandBrokerDriverPath);
    }
    case fs_management::kDiskFormatBootpart: {
      return AttachDriver(kBootpartDriverPath);
    }
    case fs_management::kDiskFormatGpt: {
      return AttachDriver(kGPTDriverPath);
    }
    case fs_management::kDiskFormatFvm: {
      return AttachDriver(kFVMDriverPath);
    }
    case fs_management::kDiskFormatMbr: {
      return AttachDriver(kMBRDriverPath);
    }
    case fs_management::kDiskFormatBlockVerity: {
      if (zx_status_t status = AttachDriver(kBlockVerityDriverPath); status != ZX_OK) {
        return status;
      }

      if (!ShouldAllowAuthoringFactory()) {
        zx::result<std::string> seal_text = VeritySeal();
        if (seal_text.is_error()) {
          FX_LOGS(ERROR) << "Couldn't get block-verity seal: " << seal_text.status_string();
          return seal_text.error_value();
        }

        return OpenBlockVerityForVerifiedRead(seal_text.value());
      }

      return ZX_OK;
    }
    case fs_management::kDiskFormatFactoryfs: {
      if (zx_status_t status = CheckFilesystem(); status != ZX_OK) {
        return status;
      }

      return MountFilesystem();
    }
    case fs_management::kDiskFormatZxcrypt: {
      return UnsealZxcrypt();
    }
    case fs_management::kDiskFormatBlobfs: {
      if (zx_status_t status = CheckFilesystem(); status != ZX_OK) {
        return status;
      }
      return MountFilesystem();
    }
    case fs_management::kDiskFormatFxfs:
    case fs_management::kDiskFormatF2fs:
    case fs_management::kDiskFormatMinfs: {
      FX_LOGS(INFO) << "mounting data partition with format " << DiskFormatString(GetFormat())
                    << ": format on corruption is "
                    << (format_on_corruption ? "enabled" : "disabled");
      if (content_format() != GetFormat()) {
        FX_LOGS(INFO) << "Data doesn't appear to be formatted yet.  Formatting...";
        if (zx_status_t status = FormatFilesystem(); status != ZX_OK) {
          return status;
        }
      } else if (zx_status_t status = CheckFilesystem(); status != ZX_OK) {
        if (!format_on_corruption) {
          FX_LOGS(INFO) << "formatting data partition on this target is disabled";
          return status;
        }
        if (zx_status_t status = FormatFilesystem(); status != ZX_OK) {
          return status;
        }
      }
      if (zx_status_t status = MountFilesystem(); status != ZX_OK) {
        FX_LOGS(ERROR) << "failed to mount filesystem: " << zx_status_get_string(status);
        if (!format_on_corruption) {
          FX_LOGS(ERROR) << "formatting minfs on this target is disabled";
          return status;
        }
        status = FormatFilesystem();
        if (status != ZX_OK) {
          return status;
        }
        return MountFilesystem();
      }
      return ZX_OK;
    }
    case fs_management::kDiskFormatFat:
    case fs_management::kDiskFormatVbmeta:
    case fs_management::kDiskFormatUnknown:
    case fs_management::kDiskFormatCount:
      return ZX_ERR_NOT_SUPPORTED;
  }
  return ZX_ERR_NOT_SUPPORTED;
}

zx_status_t BlockDevice::CheckCustomFilesystem(fs_management::DiskFormat format) const {
  zx::result cloned = component::Clone(block_, component::AssumeProtocolComposesNode);
  if (cloned.is_error()) {
    return cloned.error_value();
  }

  if (format == fs_management::kDiskFormatFxfs) {
    // Fxfs runs as a component.
    constexpr char startup_service_path[] = "/fxfs/svc/fuchsia.fs.startup.Startup";
    auto startup_client_end = component::Connect<fuchsia_fs_startup::Startup>(startup_service_path);
    if (startup_client_end.is_error()) {
      FX_PLOGS(ERROR, startup_client_end.error_value())
          << "Failed to connect to startup service at " << startup_service_path;
      return startup_client_end.error_value();
    }
    auto startup_client = fidl::WireSyncClient(std::move(*startup_client_end));
    fs_management::FsckOptions options;
    auto res = startup_client->Check(std::move(cloned.value()), options.as_check_options());
    if (!res.ok()) {
      FX_PLOGS(ERROR, res.status()) << "Failed to fsck (FIDL error)";
      return res.status();
    }
    if (res.value().is_error()) {
      FX_PLOGS(ERROR, res.value().error_value()) << "Fsck failed";
      return res.value().error_value();
    }
    return ZX_OK;
  }
  const std::string binary_path(BinaryPathForFormat(format));
  if (binary_path.empty()) {
    FX_LOGS(ERROR) << "Unsupported data format";
    return ZX_ERR_INVALID_ARGS;
  }

  fidl::ClientEnd<fuchsia_io::Node> node(cloned.value().TakeChannel());
  return RunBinary({binary_path.c_str(), "fsck", nullptr}, std::move(node));
}

// This is a destructive operation and isn't atomic (i.e. not resilient to power interruption).
zx_status_t BlockDevice::FormatCustomFilesystem(fs_management::DiskFormat format) {
  // Try mounting minfs and slurp all existing data off.
  if (content_format() == fs_management::kDiskFormatMinfs) {
    FX_LOGS(INFO) << "Attempting to read existing Minfs data";
    zx::result cloned = component::Clone(block_, component::AssumeProtocolComposesNode);
    if (cloned.is_error()) {
      return cloned.error_value();
    }
    fidl::ClientEnd<fuchsia_io::Node> node(cloned.value().TakeChannel());
    if (Copier copier = TryReadingMinfs(std::move(node)); !copier.empty()) {
      FX_LOGS(INFO) << "Successfully read Minfs data";
      source_data_.emplace(std::move(copier));
    }
  }

  FX_LOGS(INFO) << "Formatting " << DiskFormatString(format);
  zx::result cloned = component::Clone(block_, component::AssumeProtocolComposesNode);
  if (cloned.is_error()) {
    return cloned.error_value();
  }

  fidl::UnownedClientEnd<fuchsia_hardware_block_volume::Volume> volume_client(
      cloned.value().channel().borrow());
  uint64_t target_bytes = device_config_->data_max_bytes();
  const bool inside_zxcrypt = (topological_path_.find("zxcrypt") != std::string::npos);
  if (format == fs_management::kDiskFormatF2fs) {
    auto query_result = fidl::WireCall(volume_client)->GetVolumeInfo();
    if (query_result.status() != ZX_OK) {
      return query_result.status();
    }
    if (query_result.value().status != ZX_OK) {
      return query_result.value().status;
    }
    const uint64_t slice_size = query_result.value().manager->slice_size;
    uint64_t required_size = fbl::round_up(kDefaultF2fsMinBytes, slice_size);
    // f2fs always requires at least a certain size.
    if (inside_zxcrypt) {
      // Allocate an additional slice for zxcrypt metadata.
      required_size += slice_size;
    }
    target_bytes = std::max(target_bytes, required_size);
  }
  FX_LOGS(INFO) << "Resizing data volume, target = " << target_bytes << " bytes";
  auto actual_size = ResizeVolume(volume_client, target_bytes, inside_zxcrypt);
  if (actual_size.is_error()) {
    FX_PLOGS(ERROR, actual_size.status_value()) << "Failed to resize data volume";
    return actual_size.status_value();
  }
  if (format == fs_management::kDiskFormatF2fs && *actual_size < kDefaultF2fsMinBytes) {
    FX_LOGS(ERROR) << "Only allocated " << *actual_size << " bytes but needed "
                   << kDefaultF2fsMinBytes;
    return ZX_ERR_NO_SPACE;
  }
  if (*actual_size < target_bytes) {
    FX_LOGS(WARNING) << "Only allocated " << *actual_size << " bytes";
  }

  if (format == fs_management::kDiskFormatFxfs) {
    if (auto status = FormatFxfsAndInitDataVolume(std::move(cloned.value()), *device_config_);
        status.is_error()) {
      FX_PLOGS(ERROR, status.status_value()) << "Failed to format Fxfs";
      return status.status_value();
    }
  } else {
    const std::string binary_path(BinaryPathForFormat(format));
    if (binary_path.empty()) {
      FX_LOGS(ERROR) << "Unsupported data format";
      return ZX_ERR_INVALID_ARGS;
    }
    fidl::ClientEnd<fuchsia_io::Node> node(cloned.value().TakeChannel());
    if (zx_status_t status = RunBinary({binary_path.c_str(), "mkfs", nullptr}, std::move(node));
        status != ZX_OK) {
      return status;
    }
  }
  content_format_ = format_;

  return ZX_OK;
}

}  // namespace fshost
