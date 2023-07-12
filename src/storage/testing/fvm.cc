// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/testing/fvm.h"

#include <fcntl.h>
#include <fidl/fuchsia.device/cpp/wire.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/device-watcher/cpp/device-watcher.h>
#include <lib/fdio/cpp/caller.h>
#include <lib/fdio/fdio.h>
#include <lib/syslog/cpp/macros.h>

#include <fbl/unique_fd.h>
#include <ramdevice-client/ramdisk.h>

#include "src/lib/storage/fs_management/cpp/fvm.h"

namespace storage {

constexpr uuid::Uuid kTestPartGUID = {0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f,
                                      0xFF, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07};

constexpr uuid::Uuid kTestUniqueGUID = {0xFF, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07,
                                        0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x0e, 0x0f};

zx::result<> BindFvm(fidl::UnownedClientEnd<fuchsia_device::Controller> device) {
  auto resp = fidl::WireCall(device)->Bind("fvm.cm");
  auto status = zx::make_result(resp.status());
  if (status.is_ok()) {
    if (resp->is_error()) {
      status = zx::make_result(resp->error_value());
    }
  }
  if (status.is_error()) {
    FX_LOGS(ERROR) << "Could not bind disk to FVM driver: " << status.status_string();
    return status.take_error();
  }
  return zx::ok();
}

zx::result<std::string> CreateFvmInstance(const std::string& device_path, size_t slice_size) {
  zx::result device = component::Connect<fuchsia_hardware_block::Block>(device_path);
  if (device.is_error()) {
    return device.take_error();
  }
  if (zx::result status = zx::make_result(fs_management::FvmInit(device.value(), slice_size));
      status.is_error()) {
    FX_LOGS(ERROR) << "Could not format disk with FVM";
    return status.take_error();
  }

  std::string controller_path = device_path + "/device_controller";
  zx::result controller = component::Connect<fuchsia_device::Controller>(controller_path);
  if (controller.is_error()) {
    return controller.take_error();
  }
  if (zx::result status = BindFvm(controller.value()); status.is_error()) {
    return status.take_error();
  }
  std::string fvm_disk_path = device_path + "/fvm";
  if (zx::result channel = device_watcher::RecursiveWaitForFile(fvm_disk_path.c_str(), zx::sec(3));
      channel.is_error()) {
    FX_PLOGS(ERROR, channel.error_value()) << "FVM driver never appeared at " << fvm_disk_path;
    return channel.take_error();
  }

  return zx::ok(fvm_disk_path);
}

zx::result<std::string> CreateFvmPartition(const std::string& device_path, size_t slice_size,
                                           const FvmOptions& options) {
  // Format the raw device to support FVM, and bind the FVM driver to it.
  zx::result<std::string> fvm_disk_path = CreateFvmInstance(device_path, slice_size);
  if (fvm_disk_path.is_error()) {
    return fvm_disk_path.take_error();
  }

  // Open "fvm" driver
  zx::result fvm_client_end =
      component::Connect<fuchsia_hardware_block_volume::VolumeManager>(*fvm_disk_path);
  if (fvm_client_end.is_error()) {
    FX_LOGS(ERROR) << "Could not open FVM driver: " << fvm_client_end.status_string();
    return fvm_client_end.take_error();
  }

  uint64_t slice_count = options.initial_fvm_slice_count;
  uuid::Uuid type_guid = kTestPartGUID;
  if (options.type) {
    type_guid = uuid::Uuid(options.type->data());
  }

  zx::result controller = fs_management::FvmAllocatePartition(
      *fvm_client_end, slice_count, type_guid, kTestUniqueGUID, options.name, 0);
  if (controller.is_error()) {
    FX_LOGS(ERROR) << "Could not allocate FVM partition (slice count: "
                   << options.initial_fvm_slice_count << "): " << controller.status_string();
    return controller.take_error();
  }
  fidl::WireResult result = fidl::WireCall(*controller)->GetTopologicalPath();
  if (!result.ok()) {
    FX_LOGS(ERROR) << "Could not get topological path of fvm partition (fidl error): "
                   << result.FormatDescription();
    return zx::error(result.status());
  }
  fit::result response = *result;
  if (response.is_error()) {
    FX_LOGS(ERROR) << "Could not get topological path of fvm partition: "
                   << zx_status_get_string(response.error_value());
    return zx::error(response.error_value());
  }

  return zx::ok(std::string(response->path.data(), response->path.size()));
}

}  // namespace storage
