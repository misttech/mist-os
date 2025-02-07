// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/fvm/test_support.h"

#include <errno.h>
#include <fidl/fuchsia.device/cpp/wire.h>
#include <fidl/fuchsia.hardware.block.volume/cpp/wire.h>
#include <fidl/fuchsia.io/cpp/wire.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/device-watcher/cpp/device-watcher.h>
#include <lib/fdio/cpp/caller.h>
#include <lib/fidl/cpp/wire/sync_call.h>
#include <lib/fidl/cpp/wire/vector_view.h>
#include <lib/zx/time.h>
#include <zircon/status.h>

#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <string>

#include <fbl/unique_fd.h>
#include <zxtest/zxtest.h>

#include "lib/fdio/directory.h"
#include "src/storage/lib/block_client/cpp/remote_block_device.h"
#include "src/storage/lib/fs_management/cpp/fvm.h"

namespace fvm {
namespace {

constexpr char kRamdiskCtlPath[] = "sys/platform/ram-disk/ramctl";
constexpr zx::duration kDeviceWaitTime = zx::sec(30);

template <typename Protocol>
zx::result<fidl::ClientEnd<Protocol>> GetChannel(DeviceRef* device) {
  fdio_cpp::UnownedFdioCaller caller(device->devfs_root_fd());
  return component::ConnectAt<Protocol>(caller.directory(), device->path());
}

zx::result<fidl::ClientEnd<fuchsia_device::Controller>> GetController(DeviceRef* device) {
  fdio_cpp::UnownedFdioCaller caller(device->devfs_root_fd());
  std::string controller_path = std::string(device->path()).append("/device_controller");
  return component::ConnectAt<fuchsia_device::Controller>(caller.directory(), controller_path);
}

zx_status_t RebindBlockDevice(DeviceRef* device) {
  // We need to create a DirWatcher to wait for the block device's child to disappear.
  fdio_cpp::UnownedFdioCaller caller(device->devfs_root_fd());
  auto [client_end, server_end] = fidl::Endpoints<fuchsia_io::Directory>::Create();
  // TODO(https://fxbug.dev/378924259): Migrate to new Open signature.
  const fidl::OneWayStatus status =
      fidl::WireCall(caller.directory())
          ->DeprecatedOpen(fuchsia_io::OpenFlags::kDirectory, {},
                           fidl::StringView::FromExternal(device->path()),
                           fidl::ServerEnd<fuchsia_io::Node>(server_end.TakeChannel()));
  EXPECT_OK(status);
  if (!status.ok()) {
    return status.status();
  }
  zx::result watcher = device_watcher::DirWatcher::Create(client_end);
  EXPECT_OK(watcher);
  if (watcher.is_error()) {
    return watcher.error_value();
  }

  zx::result channel = GetController(device);
  if (channel.is_error()) {
    return channel.status_value();
  }
  const fidl::WireResult result = fidl::WireCall(channel.value())->Rebind({});
  if (!result.ok()) {
    ADD_FAILURE("('%s').Rebind(): %s", device->path(), result.status_string());
    return result.status();
  }
  const fit::result response = result.value();
  if (response.is_error()) {
    ADD_FAILURE("('%s').Rebind(): %s", device->path(),
                zx_status_get_string(response.error_value()));
    return response.error_value();
  }
  if (zx_status_t status =
          watcher.value().WaitForRemoval(fbl::String() /* any file */, kDeviceWaitTime);
      status != ZX_OK) {
    ADD_FAILURE("Watcher('%s').WaitForRemoval: %s", device->path(), zx_status_get_string(status));
    return status;
  }
  return ZX_OK;
}

using FidlGuid = fuchsia_hardware_block_partition::wire::Guid;

}  // namespace

// namespace

DeviceRef::DeviceRef(const fbl::unique_fd& devfs_root, const std::string& path)
    : devfs_root_(devfs_root) {
  path_.append(path);
}

std::unique_ptr<DeviceRef> DeviceRef::Create(const fbl::unique_fd& devfs_root,
                                             const std::string& device_path) {
  return std::make_unique<DeviceRef>(devfs_root, device_path);
}

std::unique_ptr<RamdiskRef> RamdiskRef::Create(const fbl::unique_fd& devfs_root,
                                               uint64_t block_size, uint64_t block_count,
                                               std::optional<zx::vmo> vmo) {
  if (!devfs_root.is_valid()) {
    ADD_FAILURE("Bad devfs root handle.");
    return nullptr;
  }

  if (block_size == 0 || block_count == 0) {
    ADD_FAILURE("Attempting to create 0 sized ramdisk.");
    return nullptr;
  }

  if (zx::result channel =
          device_watcher::RecursiveWaitForFile(devfs_root.get(), kRamdiskCtlPath, kDeviceWaitTime);
      channel.is_error()) {
    ADD_FAILURE("Failed to wait for RamCtl. Reason: %s", channel.status_string());
    return nullptr;
  }

  RamdiskClient* client;
  if (!vmo) {
    zx::vmo created_vmo;
    if (zx_status_t status = zx::vmo::create(block_size * block_count, 0, &created_vmo);
        status != ZX_OK) {
      ADD_FAILURE("Failed to create VMO. Reason: %s", zx_status_get_string(status));
      return nullptr;
    }
    vmo = std::move(created_vmo);
  }
  static uint8_t type_guid[16] = {0};
  zx::vmo duplicate_vmo;
  if (zx_status_t status = vmo->duplicate(ZX_RIGHT_SAME_RIGHTS, &duplicate_vmo); status != ZX_OK) {
    ADD_FAILURE("Failed to duplicate VMO. Reason: %s", zx_status_get_string(status));
    return nullptr;
  }
  if (zx_status_t status =
          ramdisk_create_at_from_vmo_with_params(devfs_root.get(), duplicate_vmo.release(),
                                                 block_size, type_guid, sizeof(type_guid), &client);
      status != ZX_OK) {
    ADD_FAILURE("Failed to create ramdisk. Reason: %s", zx_status_get_string(status));
    return nullptr;
  }
  const char* path = ramdisk_get_path(client);
  return std::make_unique<RamdiskRef>(devfs_root, block_size, path, *std::move(vmo), client);
}

RamdiskRef::~RamdiskRef() { ramdisk_destroy(ramdisk_client_); }

zx::result<std::unique_ptr<RamdiskRef>> RamdiskRef::Clone(uint64_t target_size) {
  zx::vmo vmo;
  if (zx_status_t status = vmo_.create_child(ZX_VMO_CHILD_SNAPSHOT, 0, target_size, &vmo);
      status != ZX_OK)
    return zx::error(status);

  auto cloned =
      RamdiskRef::Create(devfs_root_fd(), block_size_, target_size / block_size_, std::move(vmo));
  if (cloned)
    return zx::ok(std::move(cloned));
  else
    return zx::error(ZX_ERR_INTERNAL);
}

void BlockDeviceAdapter::WriteAt(const fbl::Array<uint8_t>& data, uint64_t offset) {
  zx::result channel = GetChannel<fuchsia_hardware_block::Block>(device());
  ASSERT_OK(channel.status_value());
  ASSERT_OK(block_client::SingleWriteBytes(channel.value(), data.data(), data.size(), offset));
}

void BlockDeviceAdapter::ReadAt(uint64_t offset, fbl::Array<uint8_t>* out_data) {
  zx::result channel = GetChannel<fuchsia_hardware_block::Block>(device());
  ASSERT_OK(channel.status_value());
  ASSERT_OK(
      block_client::SingleReadBytes(channel.value(), out_data->data(), out_data->size(), offset));
}

void BlockDeviceAdapter::CheckContentsAt(const fbl::Array<uint8_t>& data, uint64_t offset) {
  ASSERT_GT(data.size(), 0, "data::size must be greater than 0.");
  fbl::Array<uint8_t> device_data(new uint8_t[data.size()], data.size());
  ASSERT_NO_FAILURES(ReadAt(offset, &device_data));
  ASSERT_BYTES_EQ(device_data.data(), data.data(), data.size());
}

zx_status_t BlockDeviceAdapter::WaitUntilVisible() const {
  zx::result channel =
      device_watcher::RecursiveWaitForFile(devfs_root_.get(), device()->path(), kDeviceWaitTime);
  if (channel.is_error()) {
    ADD_FAILURE("Block device did not become visible at %s: %s", device()->path(),
                channel.status_string());
  }
  return channel.status_value();
}

zx::result<fidl::ClientEnd<fuchsia_device::Controller>> VPartitionAdapter::GetController() {
  return fvm::GetController(device());
}

std::unique_ptr<VPartitionAdapter> VPartitionAdapter::Create(const fbl::unique_fd& devfs_root,
                                                             const std::string& name,
                                                             const Guid& guid, const Guid& type) {
  if (name.empty() || type.size() == 0 || guid.size() == 0) {
    ADD_FAILURE(
        "Partition name(size=%lu), type(size=%lu) and guid(size=%lu) must be non "
        "empty.\n"
        "Partition {\n"
        "    name: %s\n"
        "    type: %s\n"
        "    guid: %s\n"
        "}",
        name.size(), type.size(), guid.size(), name.c_str(), type.ToString().c_str(),
        guid.ToString().c_str());
    return nullptr;
  }

  fs_management::PartitionMatcher matcher{
      .type_guids = {uuid::Uuid(type.data())},
      .instance_guids = {uuid::Uuid(guid.data())},
  };
  fdio_cpp::UnownedFdioCaller devfs_root_caller(devfs_root.get());
  zx::result controller =
      fs_management::OpenPartitionWithDevfs(devfs_root_caller.directory(), matcher, true);
  if (controller.is_error()) {
    ADD_FAILURE("Unable to obtain handle for partition.");
    return nullptr;
  }
  fidl::WireResult topo_path = fidl::WireCall(controller.value())->GetTopologicalPath();
  if (!topo_path.ok()) {
    ADD_FAILURE("Failed to call topo path: %s", topo_path.status_string());
    return nullptr;
  }
  if (topo_path->is_error()) {
    ADD_FAILURE("Failed to get topo path: %d", topo_path->error_value());
    return nullptr;
  }
  std::string relative_path = std::string(topo_path.value()->path.get());
  constexpr std::string_view kDevPrefix = "/dev/";
  if (!std::string_view(relative_path).starts_with(kDevPrefix)) {
    ADD_FAILURE("Bad topo path, doesn't start with /dev/: %s", relative_path.c_str());
    return nullptr;
  }
  relative_path.erase(0, kDevPrefix.size());

  return std::make_unique<VPartitionAdapter>(devfs_root, relative_path, name, guid, type);
}

zx::result<> VPartitionAdapter::Destroy() {
  return fs_management::DestroyPartitionWithDevfs(devfs_root_.get(),
                                                  {
                                                      .type_guids = {uuid::Uuid(type_.data())},
                                                      .instance_guids = {uuid::Uuid(guid_.data())},
                                                  },
                                                  false);
}

zx_status_t VPartitionAdapter::Extend(uint64_t offset, uint64_t length) {
  zx::result channel = GetChannel<fuchsia_hardware_block_volume::Volume>(device());
  if (channel.is_error()) {
    return channel.error_value();
  }
  const fidl::WireResult result = fidl::WireCall(channel.value())->Extend(offset, length);
  if (!result.ok()) {
    return result.status();
  }
  const fidl::WireResponse response = result.value();
  return response.status;
}

std::unique_ptr<FvmAdapter> FvmAdapter::Create(const fbl::unique_fd& devfs_root,
                                               uint64_t block_size, uint64_t block_count,
                                               uint64_t slice_size, DeviceRef* device) {
  return CreateGrowable(devfs_root, block_size, block_count, block_count, slice_size, device);
}

std::unique_ptr<FvmAdapter> FvmAdapter::CreateGrowable(const fbl::unique_fd& devfs_root,
                                                       uint64_t block_size,
                                                       uint64_t initial_block_count,
                                                       uint64_t maximum_block_count,
                                                       uint64_t slice_size, DeviceRef* device) {
  if (device == nullptr) {
    ADD_FAILURE("Create requires non-null device pointer.");
    return nullptr;
  }

  {
    zx::result channel = GetChannel<fuchsia_hardware_block::Block>(device);
    if (channel.is_error()) {
      ADD_FAILURE("ConnectAt(%s): %s", device->path(), channel.status_string());
      return nullptr;
    }
    if (zx_status_t status =
            fs_management::FvmInitPreallocated(channel.value(), initial_block_count * block_size,
                                               maximum_block_count * block_size, slice_size);
        status != ZX_OK) {
      ADD_FAILURE("FvmInitPreallocated(%s): %s", device->path(), zx_status_get_string(status));
      return nullptr;
    }
  }

  return Bind(devfs_root, device);
}

std::unique_ptr<FvmAdapter> FvmAdapter::Bind(const fbl::unique_fd& devfs_root, DeviceRef* device) {
  {
    zx::result channel = GetController(device);
    if (channel.is_error()) {
      ADD_FAILURE("ConnectAt(%s): %s", device->path(), channel.status_string());
      return nullptr;
    }
    const fidl::WireResult result =
        fidl::WireCall(channel.value())->Bind(fidl::StringView(kFvmDriverLib));
    if (!result.ok()) {
      ADD_FAILURE("Binding FVM driver failed: %s", result.FormatDescription().c_str());
      return nullptr;
    }
    const fit::result response = result.value();
    if (response.is_error()) {
      ADD_FAILURE("Binding FVM driver failed: %s", zx_status_get_string(response.error_value()));
      return nullptr;
    }
  }

  fbl::StringBuffer<kPathMax> fvm_path;
  fvm_path.AppendPrintf("%s/fvm", device->path());

  if (zx::result channel =
          device_watcher::RecursiveWaitForFile(devfs_root.get(), fvm_path.c_str(), kDeviceWaitTime);
      channel.is_error()) {
    ADD_FAILURE("Loading FVM driver: %s", channel.status_string());
    return nullptr;
  }
  return std::make_unique<FvmAdapter>(devfs_root, fvm_path.c_str(), device);
}

zx_status_t FvmAdapter::AddPartition(const fbl::unique_fd& devfs_root, const std::string& name,
                                     const Guid& guid, const Guid& type, uint64_t slice_count,
                                     std::unique_ptr<VPartitionAdapter>* out_vpartition) {
  FidlGuid fidl_guid, fidl_type;
  memcpy(fidl_guid.value.data(), guid.data(), guid.size());
  memcpy(fidl_type.value.data(), type.data(), type.size());

  zx::result channel = GetChannel<fuchsia_hardware_block_volume::VolumeManager>(this);
  if (channel.is_error()) {
    return channel.status_value();
  }
  const fidl::WireResult result = fidl::WireCall(channel.value())
                                      ->AllocatePartition(slice_count, fidl_type, fidl_guid,
                                                          fidl::StringView::FromExternal(name), 0u);
  if (!result.ok()) {
    return result.status();
  }
  const fidl::WireResponse response = result.value();
  if (zx_status_t status = response.status; status != ZX_OK) {
    return status;
  }

  auto vpartition = VPartitionAdapter::Create(devfs_root, name, guid, type);
  if (vpartition == nullptr) {
    return ZX_ERR_INVALID_ARGS;
  }
  if (zx_status_t status = vpartition->WaitUntilVisible(); status != ZX_OK) {
    return status;
  }

  if (out_vpartition != nullptr) {
    *out_vpartition = std::move(vpartition);
  }

  return ZX_OK;
}

zx_status_t FvmAdapter::Rebind() {
  if (zx_status_t status = RebindBlockDevice(block_device_); status != ZX_OK) {
    ADD_FAILURE("FvmAdapter block device rebind failed.");
    return status;
  }

  // Bind the FVM to the block device.
  zx::result channel = GetController(block_device_);
  if (channel.is_error()) {
    return channel.status_value();
  }
  const fidl::WireResult result =
      fidl::WireCall(channel.value())->Bind(fidl::StringView(kFvmDriverLib));
  if (!result.ok()) {
    ADD_FAILURE("Rebinding FVM driver failed: %s", result.FormatDescription().c_str());
    return result.status();
  }
  const fit::result response = result.value();
  if (response.is_error()) {
    ADD_FAILURE("Rebinding FVM driver failed: %s", zx_status_get_string(response.error_value()));
    return response.error_value();
  }

  // Wait for FVM driver to become visible.
  if (zx::result channel =
          device_watcher::RecursiveWaitForFile(devfs_root_.get(), path(), kDeviceWaitTime);
      channel.is_error()) {
    ADD_FAILURE("Loading FVM driver: %s", channel.status_string());
    return channel.status_value();
  }

  return ZX_OK;
}

zx_status_t FvmAdapter::Query(VolumeManagerInfo* out_info) const {
  fdio_cpp::UnownedFdioCaller caller(devfs_root_.get());
  zx::result<fidl::ClientEnd<fuchsia_hardware_block_volume::VolumeManager>> volume_manager =
      component::ConnectAt<fuchsia_hardware_block_volume::VolumeManager>(caller.directory(),
                                                                         path());
  if (volume_manager.is_error()) {
    ADD_FAILURE("Could not open FVM Volume Manager: %s\n",
                zx_status_get_string(volume_manager.error_value()));
    return volume_manager.error_value();
  }
  zx::result info = fs_management::FvmQuery(volume_manager.value());
  if (info.is_error()) {
    return info.error_value();
  }
  *out_info = info.value();
  return ZX_OK;
}

fbl::Array<uint8_t> MakeRandomBuffer(size_t size, unsigned int* seed) {
  fbl::Array data(new uint8_t[size], size);

  for (size_t byte = 0; byte < size; ++byte) {
    data[byte] = static_cast<uint8_t>(rand_r(seed));
  }

  return data;
}

bool IsConsistentAfterGrowth(const VolumeManagerInfo& before, const VolumeManagerInfo& after) {
  // Frowing a FVM should not allocate any slices nor should it change the slice size.
  return before.slice_size == after.slice_size &&
         before.assigned_slice_count == after.assigned_slice_count;
}

}  // namespace fvm
