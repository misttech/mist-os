// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/storage/lib/paver/block-devices.h"

#include <dirent.h>
#include <lib/fdio/cpp/caller.h>
#include <lib/fdio/directory.h>
#include <lib/fdio/watcher.h>
#include <lib/fit/defer.h>
#include <zircon/assert.h>
#include <zircon/status.h>

#include <fbl/unique_fd.h>

#include "src/storage/lib/paver/pave-logging.h"

namespace paver {

DevfsVolumeConnector::DevfsVolumeConnector(fidl::ClientEnd<fuchsia_device::Controller> controller)
    : controller_(std::move(controller)) {}

zx::result<fidl::ClientEnd<fuchsia_hardware_block_volume::Volume>> DevfsVolumeConnector::Connect() {
  zx::result endpoints = fidl::CreateEndpoints<fuchsia_hardware_block_volume::Volume>();
  if (endpoints.is_error()) {
    return endpoints.take_error();
  }
  auto [client, server] = std::move(*endpoints);
  if (fidl::OneWayStatus result = controller_->ConnectToDeviceFidl(server.TakeChannel());
      !result.ok()) {
    return zx::error(result.status());
  }
  return zx::ok(std::move(client));
}

fidl::UnownedClientEnd<fuchsia_device::Controller> DevfsVolumeConnector::Controller() {
  return controller_.client_end().borrow();
}

fidl::ClientEnd<fuchsia_device::Controller> DevfsVolumeConnector::TakeController() {
  return controller_.TakeClientEnd();
}

PathBasedVolumeConnector::PathBasedVolumeConnector(fbl::unique_fd dirfd, std::string path)
    : dirfd_(std::move(dirfd)), path_(std::move(path)) {}

zx::result<fidl::ClientEnd<fuchsia_hardware_block_volume::Volume>>
PathBasedVolumeConnector::Connect() {
  fdio_cpp::UnownedFdioCaller caller(dirfd_);
  zx::result endpoints = fidl::CreateEndpoints<fuchsia_hardware_block_volume::Volume>();
  if (endpoints.is_error()) {
    return endpoints.take_error();
  }
  auto [client, server] = std::move(*endpoints);
  if (zx_status_t status = fdio_service_connect_at(caller.borrow_channel(), path_.c_str(),
                                                   server.TakeChannel().release());
      status != ZX_OK) {
    return zx::error(status);
  }
  return zx::ok(std::move(client));
}

fidl::UnownedClientEnd<fuchsia_device::Controller> PathBasedVolumeConnector::Controller() {
  ZX_ASSERT_MSG(false, "Called Controller on a non-DevfsVolumeConnector");
}

fidl::ClientEnd<fuchsia_device::Controller> PathBasedVolumeConnector::TakeController() {
  ZX_ASSERT_MSG(false, "Called TakeController on a non-DevfsVolumeConnector");
}

BlockDevices::BlockDevices(fbl::unique_fd devfs_root, fbl::unique_fd partitions_root)
    : devfs_root_(std::move(devfs_root)), partitions_root_(std::move(partitions_root)) {}

zx::result<BlockDevices> BlockDevices::Create(fbl::unique_fd devfs_root,
                                              fbl::unique_fd partitions_root) {
  if (!partitions_root) {
    // It's OK to swallow errors here, /partitions isn't always available.
    [[maybe_unused]]
    zx_status_t status = fdio_open_fd("/partitions", 0, partitions_root.reset_and_get_address());
  }
  if (!devfs_root) {
    if (zx_status_t status = fdio_open_fd("/dev", 0, devfs_root.reset_and_get_address());
        status != ZX_OK) {
      ERROR("Failed to open /dev: %s\n", zx_status_get_string(status));
      return zx::error(status);
    }
  }
  return zx::ok(BlockDevices(std::move(devfs_root), std::move(partitions_root)));
}

BlockDevices BlockDevices::CreateEmpty() { return BlockDevices({}, {}); }

BlockDevices BlockDevices::Duplicate() const {
  return BlockDevices(devfs_root_.duplicate(), partitions_root_.duplicate());
}

zx::result<std::unique_ptr<VolumeConnector>> BlockDevices::OpenPartition(
    fit::function<bool(const zx::channel&)> filter) const {
  if (!partitions_root_ && !devfs_root_) {
    return zx::error(ZX_ERR_NOT_FOUND);
  }
  fbl::unique_fd dir_fd;
  if (partitions_root_) {
    dir_fd = partitions_root_.duplicate();
  } else {
    constexpr const char kDevfsPathSuffix[] = "class/block";
    if (zx_status_t status =
            fdio_open_fd_at(devfs_root_.get(), kDevfsPathSuffix,
                            static_cast<uint32_t>(fuchsia_io::OpenFlags::kDirectory),
                            dir_fd.reset_and_get_address());
        status != ZX_OK) {
      return zx::error(status);
    }
  }

  DIR* dir = fdopendir(dir_fd.duplicate().release());
  if (dir == nullptr) {
    ERROR("Cannot inspect block devices: %s\n", strerror(errno));
    return zx::error(ZX_ERR_INTERNAL);
  }
  const auto closer = fit::defer([dir]() { closedir(dir); });

  struct dirent* de;
  while ((de = readdir(dir)) != nullptr) {
    LOG("Entry %s\n", std::string(de->d_name).c_str());
    if (std::string_view{de->d_name} == ".") {
      continue;
    }
    fdio_cpp::UnownedFdioCaller caller(dir_fd);
    zx::channel partition_local, partition_remote;
    if (zx_status_t status = zx::channel::create(0, &partition_local, &partition_remote);
        status != ZX_OK) {
      return zx::error(status);
    }
    std::string filename(de->d_name, strnlen(de->d_name, sizeof(de->d_name)));
    if (partitions_root_) {
      filename.append("/block");
    }
    if (zx_status_t status = fdio_service_connect_at(caller.borrow_channel(), filename.c_str(),
                                                     partition_remote.release());
        status != ZX_OK) {
      return zx::error(status);
    }
    if (!filter(partition_local)) {
      continue;
    }

    if (partitions_root_) {
      return zx::ok(
          std::make_unique<PathBasedVolumeConnector>(std::move(dir_fd), std::move(filename)));
    }

    fidl::ClientEnd<fuchsia_device::Controller> controller;
    zx::result controller_server = fidl::CreateEndpoints(&controller);
    if (controller_server.is_error()) {
      return controller_server.take_error();
    }
    std::string controller_path = filename + "/device_controller";
    if (zx_status_t status =
            fdio_service_connect_at(caller.borrow_channel(), controller_path.c_str(),
                                    controller_server->TakeChannel().release());
        status != ZX_OK) {
      ERROR("Failed to connect to device_controller: %s", zx_status_get_string(status));
      return zx::error(status);
    }
    return zx::ok(std::make_unique<DevfsVolumeConnector>(std::move(controller)));
  }

  ERROR("Partition not found!");
  return zx::error(ZX_ERR_NOT_FOUND);
}

zx::result<std::unique_ptr<VolumeConnector>> BlockDevices::WaitForPartition(
    fit::function<bool(const zx::channel&)> filter, zx_duration_t timeout,
    const char* devfs_suffix) const {
  if (!partitions_root_ && !devfs_root_) {
    return zx::error(ZX_ERR_NOT_FOUND);
  }
  struct CallbackInfo {
    std::unique_ptr<VolumeConnector> out_partition;
    fit::function<bool(const zx::channel&)> filter;
    bool is_partitions_dir;
  };

  CallbackInfo info = {
      .out_partition = {},
      .filter = std::move(filter),
      .is_partitions_dir = partitions_root_.is_valid(),
  };

  auto cb = [](int dirfd, int event, const char* filename, void* cookie) {
    if (event != WATCH_EVENT_ADD_FILE) {
      return ZX_OK;
    }
    if ((strcmp(filename, ".") == 0) || strcmp(filename, "..") == 0) {
      return ZX_OK;
    }
    auto info = static_cast<CallbackInfo*>(cookie);
    auto path = info->is_partitions_dir ? std::string(filename) + "/block" : std::string(filename);
    fdio_cpp::UnownedFdioCaller caller(dirfd);

    zx::channel partition_local, partition_remote;
    if (zx_status_t status = zx::channel::create(0, &partition_local, &partition_remote);
        status != ZX_OK) {
      return status;
    }
    if (zx_status_t status = fdio_service_connect_at(caller.borrow_channel(), path.c_str(),
                                                     partition_remote.release());
        status != ZX_OK) {
      return status;
    }
    if (!info->filter(partition_local)) {
      return ZX_OK;
    }

    if (info->is_partitions_dir) {
      int dupfd = dup(dirfd);
      if (dupfd < 0) {
        return ZX_ERR_BAD_HANDLE;
      }
      info->out_partition =
          std::make_unique<PathBasedVolumeConnector>(fbl::unique_fd(dupfd), std::move(path));
      return ZX_ERR_STOP;
    }

    fidl::ClientEnd<fuchsia_device::Controller> controller;
    zx::result controller_server = fidl::CreateEndpoints(&controller);
    if (controller_server.is_error()) {
      return controller_server.status_value();
    }
    std::string controller_path = std::string(filename) + "/device_controller";
    if (zx_status_t status =
            fdio_service_connect_at(caller.borrow_channel(), controller_path.c_str(),
                                    controller_server->TakeChannel().release());
        status != ZX_OK) {
      return status;
    }
    info->out_partition = std::make_unique<DevfsVolumeConnector>(std::move(controller));
    return ZX_ERR_STOP;
  };

  fbl::unique_fd dir_fd;
  if (partitions_root_) {
    dir_fd = partitions_root_.duplicate();
  } else {
    if (zx_status_t status =
            fdio_open_fd_at(devfs_root_.get(), devfs_suffix,
                            static_cast<uint32_t>(fuchsia_io::OpenFlags::kDirectory),
                            dir_fd.reset_and_get_address());
        status != ZX_OK) {
      ERROR("Failed to open /dev/%s: %s\n", devfs_suffix, zx_status_get_string(status));
      return zx::error(status);
    }
  }

  zx_time_t deadline = zx_deadline_after(timeout);
  if (zx_status_t status = fdio_watch_directory(dir_fd.get(), cb, deadline, &info);
      status != ZX_ERR_STOP) {
    return zx::error(status);
  }
  return zx::ok(std::move(info.out_partition));
}

}  // namespace paver
