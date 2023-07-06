// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "factory_reset.h"

#include <dirent.h>
#include <fcntl.h>
#include <fidl/fuchsia.fshost/cpp/wire.h>
#include <fidl/fuchsia.hardware.block/cpp/wire.h>
#include <lib/component/incoming/cpp/clone.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/fdio/cpp/caller.h>
#include <lib/fdio/fdio.h>
#include <lib/fit/defer.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/channel.h>
#include <zircon/errors.h>
#include <zircon/status.h>

#include "src/lib/storage/block_client/cpp/remote_block_device.h"
#include "src/lib/storage/fs_management/cpp/format.h"
#include "src/security/lib/kms-stateless/kms-stateless.h"
#include "src/security/lib/zxcrypt/client.h"

namespace factory_reset {

const char* kBlockPath = "class/block";

zx_status_t ShredZxcryptDevice(fidl::ClientEnd<fuchsia_device::Controller> device,
                               fbl::unique_fd devfs_root_fd) {
  zxcrypt::VolumeManager volume(std::move(device), std::move(devfs_root_fd));

  // Note: the access to /dev/sys/platform from the manifest is load-bearing
  // here, because we can only find the related zxcrypt device for a particular
  // block device via appending "/zxcrypt" to its topological path, and the
  // canonical topological path sits under sys/platform.
  zx::channel driver_chan;
  if (zx_status_t status = volume.OpenClient(zx::sec(5), driver_chan); status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "Couldn't open channel to zxcrypt volume manager";
    return status;
  }

  zxcrypt::EncryptedVolumeClient zxc_manager(std::move(driver_chan));
  if (zx_status_t status = zxc_manager.Shred(); status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "Couldn't shred volume";
    return status;
  }

  return ZX_OK;
}

zx_status_t ShredFxfsDevice(const fidl::ClientEnd<fuchsia_hardware_block::Block>& device) {
  // Overwrite the magic bytes of both superblocks.
  //
  // Note: This may occasionally be racy. Superblocks may be writen after
  // the overwrite below but before reboot. When we move this to fshost, we
  // will have access to the running filesystem and can wait for shutdown with
  // something like:
  //   fdio_cpp::FdioCaller caller(std::move(fd));
  //   if (zx::result<> status = fs_management::Shutdown(caller.directory()); !status.is_ok()) {
  //     return status.error_value();
  //   }
  // TODO(https://fxbug.dev/98889): Perform secure erase once we have keybag support.
  const fidl::WireResult result = fidl::WireCall(device)->GetInfo();
  if (!result.ok()) {
    FX_PLOGS(ERROR, result.status()) << "Failed to fetch block size";
    return result.status();
  }
  const fit::result response = result.value();
  if (response.is_error()) {
    FX_PLOGS(ERROR, response.error_value()) << "Failed to fetch block size";
    return response.error_value();
  }
  size_t block_size = response.value()->info.block_size;
  std::unique_ptr<uint8_t[]> block = std::make_unique<uint8_t[]>(block_size);
  memset(block.get(), 0, block_size);
  for (off_t offset : {0L, 512L << 10}) {
    if (zx_status_t status =
            block_client::SingleWriteBytes(device, block.get(), block_size, offset);
        status != ZX_OK) {
      FX_PLOGS(ERROR, status) << "Failed to write to fxfs device at offset " << offset;
      return status;
    }
  }
  return ZX_OK;
}

FactoryReset::FactoryReset(async_dispatcher_t* dispatcher,
                           fidl::ClientEnd<fuchsia_io::Directory> dev,
                           fidl::ClientEnd<fuchsia_hardware_power_statecontrol::Admin> admin,
                           fidl::ClientEnd<fuchsia_fshost::Admin> fshost_admin)
    : dev_(std::move(dev)),
      admin_(std::move(admin), dispatcher),
      fshost_admin_(std::move(fshost_admin), dispatcher) {}

void FactoryReset::Shred(fit::callback<void(zx_status_t)> callback) const {
  // First try and shred the data volume using fshost.
  auto cb = [this, callback = std::move(callback)](const auto& result) mutable {
    callback([this, &result]() {
      if (result.ok()) {
        const fit::result response = result.value();
        if (response.is_ok()) {
          FX_LOGS(INFO) << "fshost ShredDataVolume succeeded";
          return ZX_OK;
        }
        if (response.is_error()) {
          if (response.error_value() != ZX_ERR_NOT_SUPPORTED) {
            FX_PLOGS(ERROR, response.error_value()) << "fshost ShredDataVolume failed";
          }
        }
      } else {
        FX_LOGS(ERROR) << "Failed to call ShredDataVolume: " << result.FormatDescription();
      }
      // Fall back to shredding all zxcrypt devices and Fxfs volumes...
      zx::result block_dir = component::ConnectAt<fuchsia_io::Directory>(dev_, kBlockPath);
      if (block_dir.is_error()) {
        FX_PLOGS(ERROR, block_dir.error_value()) << "Failed to open '" << kBlockPath << "'";
        return block_dir.error_value();
      }
      int fd;
      if (zx_status_t status = fdio_fd_create(block_dir.value().TakeChannel().release(), &fd);
          status != ZX_OK) {
        FX_PLOGS(ERROR, status) << "Failed to create fd from '" << kBlockPath << "'";
        return status;
      }
      DIR* const dir = fdopendir(fd);
      auto cleanup = fit::defer([dir]() { closedir(dir); });
      fdio_cpp::UnownedFdioCaller caller(dirfd(dir));
      // Attempts to shred every zxcrypt volume found.
      while (true) {
        dirent* de = readdir(dir);
        if (de == nullptr) {
          return ZX_OK;
        }
        if (std::string_view(de->d_name) == ".") {
          continue;
        }
        zx::result block =
            component::ConnectAt<fuchsia_hardware_block::Block>(caller.directory(), de->d_name);
        if (block.is_error()) {
          FX_PLOGS(ERROR, block.status_value()) << "Error opening " << de->d_name;
          continue;
        }

        std::string controller_path = std::string(de->d_name) + "/device_controller";
        zx::result block_controller =
            component::ConnectAt<fuchsia_device::Controller>(caller.directory(), controller_path);
        if (block_controller.is_error()) {
          FX_PLOGS(ERROR, block_controller.status_value()) << "Error opening " << controller_path;
          continue;
        }
        zx_status_t status;
        switch (fs_management::DetectDiskFormat(block.value())) {
          case fs_management::kDiskFormatZxcrypt: {
            fbl::unique_fd dev_fd;
            {
              zx::result dev = component::Clone(dev_);
              if (dev.is_error()) {
                FX_PLOGS(ERROR, dev.error_value()) << "Error cloning connection to /dev";
                continue;
              }
              if (zx_status_t status = fdio_fd_create(dev.value().TakeChannel().release(),
                                                      dev_fd.reset_and_get_address());
                  status != ZX_OK) {
                FX_PLOGS(ERROR, status) << "Error creating file descriptor from /dev";
                continue;
              }
            }

            status = ShredZxcryptDevice(std::move(block_controller.value()), std::move(dev_fd));
            break;
          }
          case fs_management::kDiskFormatFxfs: {
            status = ShredFxfsDevice(block.value());
            break;
          }
          default:
            continue;
        }
        if (status != ZX_OK) {
          FX_PLOGS(ERROR, status) << "Error shredding " << de->d_name;
          return status;
        }
        FX_LOGS(INFO) << "Successfully shredded " << de->d_name;
      }
    }());
  };
  fshost_admin_->ShredDataVolume().ThenExactlyOnce(std::move(cb));
}

void FactoryReset::Reset(fit::callback<void(zx_status_t)> callback) {
  FX_LOGS(INFO) << "Reset called. Starting shred";
  Shred([this, callback = std::move(callback)](zx_status_t status) mutable {
    if (status != ZX_OK) {
      FX_PLOGS(ERROR, status) << "Shred failed";
      callback(status);
      return;
    }
    FX_LOGS(INFO) << "Finished shred";

    uint8_t key_info[kms_stateless::kExpectedKeyInfoSize] = "zxcrypt";
    switch (zx_status_t status = kms_stateless::RotateHardwareDerivedKeyFromService(key_info);
            status) {
      case ZX_OK:
        break;
      case ZX_ERR_NOT_SUPPORTED:
        FX_LOGS(WARNING)
            << "FactoryReset: The device does not support rotatable hardware keys. Ignoring";
        break;
      default:
        FX_PLOGS(ERROR, status) << "FactoryReset: RotateHardwareDerivedKey() failed";
        callback(status);
        return;
    }
    // Reboot to initiate the recovery.
    FX_LOGS(INFO) << "Requesting reboot...";
    auto cb = [callback = std::move(callback)](const auto& result) mutable {
      if (!result.ok()) {
        FX_PLOGS(ERROR, result.status()) << "Reboot call failed";
        callback(result.status());
        return;
      }
      const auto& response = result.value();
      if (response.is_error()) {
        FX_PLOGS(ERROR, response.error_value()) << "Reboot returned error";
        callback(response.error_value());
        return;
      }
      callback(ZX_OK);
    };
    admin_->Reboot(fuchsia_hardware_power_statecontrol::wire::RebootReason::kFactoryDataReset)
        .ThenExactlyOnce(std::move(cb));
  });
}

void FactoryReset::Reset(ResetCompleter::Sync& completer) {
  Reset([completer = completer.ToAsync()](zx_status_t status) mutable { completer.Reply(status); });
}

}  // namespace factory_reset
