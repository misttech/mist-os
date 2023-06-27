// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/block/drivers/block-verity/verified-volume-client.h"

#include <fidl/fuchsia.device/cpp/wire.h>
#include <fidl/fuchsia.hardware.block.verified/cpp/wire.h>
#include <lib/device-watcher/cpp/device-watcher.h>
#include <lib/fdio/cpp/caller.h>
#include <lib/fdio/fdio.h>
#include <zircon/status.h>

#include <cstring>

#include <fbl/string.h>
#include <fbl/string_buffer.h>

#include "lib/stdcompat/string_view.h"

namespace block_verity {
namespace {

const char* kDriverLib = "block-verity.cm";

zx_status_t BindVerityDriver(fidl::UnownedClientEnd<fuchsia_device::Controller> channel) {
  const fidl::WireResult result =
      fidl::WireCall(channel)->Bind(fidl::StringView::FromExternal(kDriverLib));
  if (!result.ok()) {
    return result.status();
  }
  const fit::result response = result.value();
  if (response.is_error()) {
    return response.error_value();
  }
  return ZX_OK;
}

zx::result<fbl::String> RelativeTopologicalPath(
    fidl::UnownedClientEnd<fuchsia_device::Controller> channel) {
  const fidl::WireResult result = fidl::WireCall(channel)->GetTopologicalPath();
  if (!result.ok()) {
    return zx::error(result.status());
  }
  const fit::result response = result.value();
  if (response.is_error()) {
    return zx::error(response.error_value());
  }
  std::string_view path = response.value()->path.get();

  constexpr std::string_view kSlashDevSlash = "/dev/";
  if (!cpp20::starts_with(path, kSlashDevSlash)) {
    return zx::error(ZX_ERR_INTERNAL);
  }
  return zx::ok(path.substr(kSlashDevSlash.size()));
}

}  // namespace

VerifiedVolumeClient::VerifiedVolumeClient(
    fidl::ClientEnd<fuchsia_hardware_block_verified::DeviceManager> verity_chan,
    fidl::ClientEnd<fuchsia_device::Controller> verity_controller, fbl::unique_fd devfs_root_fd)
    : verity_chan_(std::move(verity_chan)),
      verity_controller_(std::move(verity_controller)),
      devfs_root_fd_(std::move(devfs_root_fd)) {}

zx::result<std::unique_ptr<VerifiedVolumeClient>> VerifiedVolumeClient::CreateFromBlockDevice(
    fidl::UnownedClientEnd<fuchsia_device::Controller> device, fbl::unique_fd devfs_root_fd,
    Disposition disposition, const zx::duration& timeout) {
  // Bind the driver if called for by `disposition`.
  if (disposition == kDriverNeedsBinding) {
    if (zx_status_t status = BindVerityDriver(device); status != ZX_OK) {
      printf("VerifiedVolumeClient: couldn't bind driver: %s", zx_status_get_string(status));
      return zx::error(status);
    }
  }

  // Compute the path at which we expect to see the verity child device appear.
  zx::result block_dev_path = RelativeTopologicalPath(device);
  if (block_dev_path.is_error()) {
    printf("VerifiedVolumeClient: could not compute relative path: %s\n",
           block_dev_path.status_string());
    return block_dev_path.take_error();
  }
  fbl::String verity_path = fbl::String::Concat({block_dev_path.value(), "/verity"});

  // Wait for the device to appear.
  zx::result channel =
      device_watcher::RecursiveWaitForFile(devfs_root_fd.get(), verity_path.c_str(), timeout);
  if (channel.is_error()) {
    printf("VerifiedVolumeClient: verity device failed to appear: %s\n", channel.status_string());
    return channel.take_error();
  }

  fbl::String controller_path = fbl::String::Concat({verity_path, "/device_controller"});
  zx::result controller_channel =
      device_watcher::RecursiveWaitForFile(devfs_root_fd.get(), controller_path.c_str(), timeout);
  if (controller_channel.is_error()) {
    printf("VerifiedVolumeClient: verity controller failed to appear: %s\n",
           controller_channel.status_string());
    return controller_channel.take_error();
  }

  return zx::ok(std::make_unique<VerifiedVolumeClient>(
      fidl::ClientEnd<fuchsia_hardware_block_verified::DeviceManager>{std::move(channel.value())},
      fidl::ClientEnd<fuchsia_device::Controller>{std::move(controller_channel.value())},
      std::move(devfs_root_fd)));
}

zx_status_t VerifiedVolumeClient::OpenForAuthoring(const zx::duration& timeout,
                                                   fbl::unique_fd& mutable_block_fd_out) {
  // make FIDL call to open in authoring mode
  fidl::Arena allocator;

  // Request the device be opened for writes
  auto open_resp =
      fidl::WireCall(verity_chan_)
          ->OpenForWrite(
              fuchsia_hardware_block_verified::wire::Config::Builder(allocator)
                  .hash_function(fuchsia_hardware_block_verified::wire::HashFunction::kSha256)
                  .block_size(fuchsia_hardware_block_verified::wire::BlockSize::kSize4096)
                  .Build());
  if (open_resp.status() != ZX_OK) {
    return open_resp.status();
  }
  if (open_resp->is_error()) {
    return open_resp->error_value();
  }

  // Compute path of expected `mutable` child device via relative topological path
  zx::result verity_path = RelativeTopologicalPath(verity_controller_);
  if (verity_path.is_error()) {
    printf("VerifiedVolumeClient: could not compute relative path: %s\n",
           verity_path.status_string());
    return verity_path.error_value();
  }
  fbl::String mutable_path = fbl::String::Concat({verity_path.value(), "/mutable"});

  // Wait for the `mutable` child device to appear
  if (zx::result channel =
          device_watcher::RecursiveWaitForFile(devfs_root_fd_.get(), mutable_path.c_str(), timeout);
      channel.is_error()) {
    printf("VerifiedVolumeClient: mutable device failed to appear: %s\n", channel.status_string());
    return channel.error_value();
  }

  // Then wait for the `block` child of that mutable device
  fbl::String mutable_block_path = fbl::String::Concat({mutable_path, "/block"});
  zx::result channel = device_watcher::RecursiveWaitForFile(devfs_root_fd_.get(),
                                                            mutable_block_path.c_str(), timeout);
  if (channel.is_error()) {
    printf("VerifiedVolumeClient: mutable block device failed to appear: %s\n",
           channel.status_string());
    return channel.error_value();
  }

  // Open child device and return
  if (zx_status_t status =
          fdio_fd_create(channel.value().release(), mutable_block_fd_out.reset_and_get_address());
      status != ZX_OK) {
    printf("VerifiedVolumeClient: failed to open %s: %s\n", mutable_block_path.c_str(),
           zx_status_get_string(status));
    return status;
  }
  return ZX_OK;
}

zx_status_t VerifiedVolumeClient::Close() {
  // Close the device cleanly
  auto close_resp = fidl::WireCall(verity_chan_)->Close();
  if (close_resp.status() != ZX_OK) {
    return close_resp.status();
  }
  if (close_resp->is_error()) {
    return close_resp->error_value();
  }

  return ZX_OK;
}

zx_status_t VerifiedVolumeClient::CloseAndGenerateSeal(
    fidl::AnyArena& arena,
    fuchsia_hardware_block_verified::wire::DeviceManagerCloseAndGenerateSealResponse* out) {
  // We use the caller-provided buffer FIDL call style because the caller
  // needs to do something with the seal returned, so we need to keep the
  // response object alive so that the caller can interact with it after this
  // function returns.
  auto seal_resp = fidl::WireCall(verity_chan_).buffer(arena)->CloseAndGenerateSeal();
  if (seal_resp.status() != ZX_OK) {
    return seal_resp.status();
  }
  if (seal_resp->is_error()) {
    return seal_resp->error_value();
  }

  *out = *seal_resp.value().value();
  return ZX_OK;
}

zx_status_t VerifiedVolumeClient::OpenForVerifiedRead(const digest::Digest& expected_seal,
                                                      const zx::duration& timeout,
                                                      fbl::unique_fd& verified_block_fd_out) {
  // make FIDL call to open in authoring mode
  fidl::Arena allocator;

  // Make a copy of the seal to send.
  fuchsia_hardware_block_verified::wire::Sha256Seal sha256_seal;
  expected_seal.CopyTo(sha256_seal.superblock_hash.begin(), sha256_seal.superblock_hash.size());

  // Request the device be opened for verified read
  auto open_resp =
      fidl::WireCall(verity_chan_)
          ->OpenForVerifiedRead(
              fuchsia_hardware_block_verified::wire::Config::Builder(allocator)
                  .hash_function(fuchsia_hardware_block_verified::wire::HashFunction::kSha256)
                  .block_size(fuchsia_hardware_block_verified::wire::BlockSize::kSize4096)
                  .Build(),
              fuchsia_hardware_block_verified::wire::Seal::WithSha256(
                  fidl::ObjectView<fuchsia_hardware_block_verified::wire::Sha256Seal>::FromExternal(
                      &sha256_seal)));
  if (open_resp.status() != ZX_OK) {
    return open_resp.status();
  }
  if (open_resp->is_error()) {
    return open_resp->error_value();
  }

  // Compute path of expected `verified` child device via relative topological path
  zx::result verity_path = RelativeTopologicalPath(verity_controller_);
  if (verity_path.is_error()) {
    printf("VerifiedVolumeClient: could not compute relative path: %s\n",
           verity_path.status_string());
    return verity_path.error_value();
  }
  fbl::String verified_path = fbl::String::Concat({verity_path.value(), "/verified"});

  // Wait for the `verified` child device to appear
  if (zx::result channel = device_watcher::RecursiveWaitForFile(devfs_root_fd_.get(),
                                                                verified_path.c_str(), timeout);
      channel.is_error()) {
    printf("VerifiedVolumeClient: verified device failed to appear: %s\n", channel.status_string());
    return channel.error_value();
  }

  // Then wait for the `block` child of that verified device
  fbl::String verified_block_path = fbl::String::Concat({verified_path, "/block"});
  zx::result channel = device_watcher::RecursiveWaitForFile(devfs_root_fd_.get(),
                                                            verified_block_path.c_str(), timeout);
  if (channel.is_error()) {
    printf("VerifiedVolumeClient: verified block device failed to appear: %s\n",
           channel.status_string());
    return channel.error_value();
  }

  if (zx_status_t status =
          fdio_fd_create(channel.value().release(), verified_block_fd_out.reset_and_get_address());
      status != ZX_OK) {
    printf("VerifiedVolumeClient: failed to open %s: %s\n", verified_block_path.c_str(),
           zx_status_get_string(status));
    return status;
  }
  return ZX_OK;
}

}  // namespace block_verity
