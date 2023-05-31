// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <fidl/fuchsia.device/cpp/wire.h>
#include <fidl/fuchsia.hardware.block/cpp/wire.h>
#include <fidl/fuchsia.hardware.ramdisk/cpp/wire.h>
#include <inttypes.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/device-watcher/cpp/device-watcher.h>
#include <lib/fdio/cpp/caller.h>
#include <lib/fdio/directory.h>
#include <lib/fdio/fd.h>
#include <lib/fdio/fdio.h>
#include <lib/fdio/watcher.h>
#include <lib/fit/defer.h>
#include <lib/zbi-format/partition.h>
#include <lib/zx/channel.h>
#include <lib/zx/time.h>
#include <lib/zx/vmo.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <sys/types.h>
#include <unistd.h>
#include <zircon/process.h>
#include <zircon/processargs.h>
#include <zircon/status.h>
#include <zircon/syscalls.h>
#include <zircon/types.h>

#include <memory>
#include <utility>

#include <fbl/string.h>
#include <fbl/string_printf.h>
#include <fbl/unique_fd.h>
#include <ramdevice-client/ramdisk.h>

constexpr char kRamctlDevPath[] = "/dev";
constexpr char kRamctlPath[] = "sys/platform/00:00:2d/ramctl";
constexpr char kBlockExtension[] = "block";
constexpr zx::duration kDeviceWaitTime = zx::sec(10);

struct ramdisk_client {
 public:
  DISALLOW_COPY_ASSIGN_AND_MOVE(ramdisk_client);

  static zx_status_t Create(int dev_root_fd, std::string_view instance_name, zx::duration duration,
                            std::unique_ptr<ramdisk_client>* out) {
    fbl::String ramdisk_path = fbl::String::Concat({kRamctlPath, "/", instance_name});
    fbl::String block_path = fbl::String::Concat({ramdisk_path, "/", kBlockExtension});
    fbl::String path;
    fbl::unique_fd dirfd;
    if (dev_root_fd > -1) {
      dirfd.reset(dup(dev_root_fd));
      path = block_path;
    } else {
      dirfd.reset(open(kRamctlDevPath, O_RDONLY | O_DIRECTORY));
      path = fbl::String::Concat({kRamctlDevPath, "/", block_path});
    }
    if (!dirfd) {
      return ZX_ERR_BAD_STATE;
    }
    fdio_cpp::UnownedFdioCaller caller(dirfd);

    zx::result channel =
        device_watcher::RecursiveWaitForFile(dirfd.get(), ramdisk_path.c_str(), duration);
    if (channel.is_error()) {
      return channel.status_value();
    }
    fidl::ClientEnd ramdisk_interface =
        fidl::ClientEnd<fuchsia_hardware_ramdisk::Ramdisk>(std::move(channel.value()));

    std::string controller_path = std::string(ramdisk_path.c_str()) + "/device_controller";
    zx::result ramdisk_controller =
        device_watcher::RecursiveWaitForFile(dirfd.get(), controller_path.c_str(), duration);
    if (ramdisk_controller.is_error()) {
      return ramdisk_controller.error_value();
    }
    fidl::ClientEnd ramdisk_controller_interface =
        fidl::ClientEnd<fuchsia_device::Controller>(std::move(ramdisk_controller.value()));

    // If binding to the block interface fails, ensure we still try to tear down the
    // ramdisk driver.
    auto cleanup = fit::defer([&ramdisk_controller_interface]() {
      ramdisk_client::DestroyByHandle(ramdisk_controller_interface);
    });

    channel = device_watcher::RecursiveWaitForFile(dirfd.get(), block_path.c_str(), duration);
    if (channel.is_error()) {
      return channel.error_value();
    }

    controller_path = std::string(block_path.c_str()) + "/device_controller";
    zx::result controller =
        device_watcher::RecursiveWaitForFile(dirfd.get(), controller_path.c_str(), duration);
    if (controller.is_error()) {
      return controller.error_value();
    }

    cleanup.cancel();
    *out = std::unique_ptr<ramdisk_client>(new ramdisk_client(
        std::move(dirfd), std::move(path), std::move(block_path), std::move(ramdisk_interface),
        std::move(ramdisk_controller_interface),
        fidl::ClientEnd<fuchsia_hardware_block::Block>{std::move(channel.value())},
        fidl::ClientEnd<fuchsia_device::Controller>(std::move(controller.value()))));
    return ZX_OK;
  }

  zx_status_t Rebind() {
    const fidl::WireResult result = fidl::WireCall(block_controller_)->Rebind({});
    if (!result.ok()) {
      return result.status();
    }
    const fit::result response = result.value();
    if (response.is_error()) {
      return response.error_value();
    }
    ramdisk_interface_.reset();

    // Ramdisk paths have the form: /dev/.../ramctl/ramdisk-xxx/block.
    // To rebind successfully, first, we rebind the "ramdisk-xxx" path,
    // and then we wait for "block" to rebind.
    char ramdisk_path[PATH_MAX];

    // Wait for the "ramdisk-xxx" path to rebind.
    {
      const char* sep = strrchr(relative_path_.c_str(), '/');
      strlcpy(ramdisk_path, relative_path_.c_str(), sep - relative_path_.c_str() + 1);
      zx::result channel =
          device_watcher::RecursiveWaitForFile(dev_root_fd_.get(), ramdisk_path, kDeviceWaitTime);
      if (channel.is_error()) {
        return channel.error_value();
      }
      ramdisk_interface_.channel() = std::move(channel.value());
    }

    // Wait for the "block" path to rebind.
    strlcpy(ramdisk_path, relative_path_.c_str(), sizeof(ramdisk_path));
    zx::result channel =
        device_watcher::RecursiveWaitForFile(dev_root_fd_.get(), ramdisk_path, kDeviceWaitTime);
    if (channel.is_error()) {
      return channel.error_value();
    }
    block_interface_.channel() = std::move(channel.value());
    return ZX_OK;
  }

  zx_status_t Destroy() {
    if (!ramdisk_interface_) {
      return ZX_ERR_BAD_STATE;
    }

    if (zx_status_t status = DestroyByHandle(ramdisk_controller_); status != ZX_OK) {
      return status;
    }
    block_interface_.reset();
    return ZX_OK;
  }

  // Destroy all open handles to the ramdisk, while leaving the ramdisk itself attached.
  // After calling this method, destroying this object will have no effect.
  zx_status_t Forget() {
    if (!ramdisk_interface_) {
      return ZX_ERR_BAD_STATE;
    }

    ramdisk_interface_.reset();
    block_interface_.reset();
    return ZX_OK;
  }

  fidl::UnownedClientEnd<fuchsia_device::Controller> controller_interface() const {
    return ramdisk_controller_;
  }

  fidl::UnownedClientEnd<fuchsia_hardware_ramdisk::Ramdisk> ramdisk_interface() const {
    return ramdisk_interface_.borrow();
  }

  fidl::UnownedClientEnd<fuchsia_hardware_block::Block> block_interface() const {
    return block_interface_.borrow();
  }

  const fbl::String& path() const { return path_; }

  ~ramdisk_client() { Destroy(); }

 private:
  ramdisk_client(fbl::unique_fd dev_root_fd, fbl::String path, fbl::String relative_path,
                 fidl::ClientEnd<fuchsia_hardware_ramdisk::Ramdisk> ramdisk_interface,
                 fidl::ClientEnd<fuchsia_device::Controller> ramdisk_controller,
                 fidl::ClientEnd<fuchsia_hardware_block::Block> block_interface,
                 fidl::ClientEnd<fuchsia_device::Controller> block_controller)
      : dev_root_fd_(std::move(dev_root_fd)),
        path_(std::move(path)),
        relative_path_(std::move(relative_path)),
        ramdisk_interface_(std::move(ramdisk_interface)),
        ramdisk_controller_(std::move(ramdisk_controller)),
        block_interface_(std::move(block_interface)),
        block_controller_(std::move(block_controller)) {}

  static zx_status_t DestroyByHandle(fidl::ClientEnd<fuchsia_device::Controller>& ramdisk) {
    const fidl::WireResult result = fidl::WireCall(ramdisk)->ScheduleUnbind();
    if (!result.ok()) {
      return result.status();
    }
    const fit::result response = result.value();
    if (response.is_error()) {
      return response.error_value();
    }
    return ZX_OK;
  }

  const fbl::unique_fd dev_root_fd_;
  // The fully qualified path.
  const fbl::String path_;
  // The path relative to dev_root_fd_.
  const fbl::String relative_path_;
  fidl::ClientEnd<fuchsia_hardware_ramdisk::Ramdisk> ramdisk_interface_;
  fidl::ClientEnd<fuchsia_device::Controller> ramdisk_controller_;
  fidl::ClientEnd<fuchsia_hardware_block::Block> block_interface_;
  fidl::ClientEnd<fuchsia_device::Controller> block_controller_;
};

static zx::result<fidl::ClientEnd<fuchsia_hardware_ramdisk::RamdiskController>> open_ramctl(
    int dev_root_fd) {
  fbl::unique_fd dirfd;
  if (dev_root_fd > -1) {
    dirfd.reset(dup(dev_root_fd));
  } else {
    dirfd.reset(open(kRamctlDevPath, O_RDONLY | O_DIRECTORY));
  }
  if (!dirfd) {
    return zx::error(ZX_ERR_BAD_STATE);
  }
  fdio_cpp::FdioCaller caller(std::move(dirfd));
  return component::ConnectAt<fuchsia_hardware_ramdisk::RamdiskController>(caller.directory(),
                                                                           kRamctlPath);
}

static zx_status_t ramdisk_create_with_guid_internal(
    int dev_root_fd, uint64_t blk_size, uint64_t blk_count,
    fidl::ObjectView<fuchsia_hardware_ramdisk::wire::Guid> type_guid, ramdisk_client** out) {
  zx::result ramctl = open_ramctl(dev_root_fd);
  if (ramctl.is_error()) {
    return ramctl.status_value();
  }

  const fidl::WireResult wire_result =
      fidl::WireCall(ramctl.value())->Create(blk_size, blk_count, type_guid);
  if (!wire_result.ok()) {
    return wire_result.status();
  }
  const fit::result result = wire_result.value();
  if (result.is_error()) {
    return result.error_value();
  }

  std::unique_ptr<ramdisk_client> client;
  if (zx_status_t status =
          ramdisk_client::Create(dev_root_fd, result->name.get(), kDeviceWaitTime, &client);
      status != ZX_OK) {
    return status;
  }
  *out = client.release();
  return ZX_OK;
}

__EXPORT
zx_status_t ramdisk_create_at(int dev_root_fd, uint64_t blk_size, uint64_t blk_count,
                              ramdisk_client** out) {
  return ramdisk_create_with_guid_internal(dev_root_fd, blk_size, blk_count, nullptr, out);
}

__EXPORT
zx_status_t ramdisk_create(uint64_t blk_size, uint64_t blk_count, ramdisk_client** out) {
  return ramdisk_create_at(/*dev_root_fd=*/-1, blk_size, blk_count, out);
}

__EXPORT
zx_status_t ramdisk_create_with_guid(uint64_t blk_size, uint64_t blk_count,
                                     const uint8_t* type_guid, size_t guid_len,
                                     ramdisk_client** out) {
  return ramdisk_create_at_with_guid(/*dev_root_fd=*/-1, blk_size, blk_count, type_guid, guid_len,
                                     out);
}

__EXPORT
zx_status_t ramdisk_create_at_with_guid(int dev_root_fd, uint64_t blk_size, uint64_t blk_count,
                                        const uint8_t* type_guid, size_t guid_len,
                                        ramdisk_client** out) {
  if (type_guid != nullptr && guid_len < ZBI_PARTITION_GUID_LEN) {
    return ZX_ERR_INVALID_ARGS;
  }
  return ramdisk_create_with_guid_internal(
      dev_root_fd, blk_size, blk_count,
      fidl::ObjectView<fuchsia_hardware_ramdisk::wire::Guid>::FromExternal(
          reinterpret_cast<fuchsia_hardware_ramdisk::wire::Guid*>(const_cast<uint8_t*>(type_guid))),
      out);
}

__EXPORT
zx_status_t ramdisk_create_from_vmo(zx_handle_t raw_vmo, ramdisk_client** out) {
  return ramdisk_create_at_from_vmo(/*dev_root_fd=*/-1, raw_vmo, out);
}

__EXPORT
zx_status_t ramdisk_create_from_vmo_with_params(zx_handle_t raw_vmo, uint64_t block_size,
                                                const uint8_t* type_guid, size_t guid_len,
                                                ramdisk_client** out) {
  return ramdisk_create_at_from_vmo_with_params(/*dev_root_fd=*/-1, raw_vmo, block_size, type_guid,
                                                guid_len, out);
}

__EXPORT
zx_status_t ramdisk_create_at_from_vmo(int dev_root_fd, zx_handle_t vmo, ramdisk_client** out) {
  return ramdisk_create_at_from_vmo_with_params(dev_root_fd, vmo, /*block_size*/ 0,
                                                /*type_guid*/ nullptr, /*guid_len*/ 0, out);
}

__EXPORT
zx_status_t ramdisk_create_at_from_vmo_with_params(int dev_root_fd, zx_handle_t raw_vmo,
                                                   uint64_t block_size, const uint8_t* type_guid,
                                                   size_t guid_len, ramdisk_client** out) {
  if (type_guid != nullptr && guid_len < ZBI_PARTITION_GUID_LEN) {
    return ZX_ERR_INVALID_ARGS;
  }
  zx::vmo vmo(raw_vmo);
  zx::result ramctl = open_ramctl(dev_root_fd);
  if (ramctl.is_error()) {
    return ramctl.status_value();
  }

  const fidl::WireResult wire_result =
      fidl::WireCall(ramctl.value())
          ->CreateFromVmoWithParams(
              std::move(vmo), block_size,
              fidl::ObjectView<fuchsia_hardware_ramdisk::wire::Guid>::FromExternal(
                  reinterpret_cast<fuchsia_hardware_ramdisk::wire::Guid*>(
                      const_cast<uint8_t*>(type_guid))));
  if (!wire_result.ok()) {
    return wire_result.status();
  }
  const fit::result result = wire_result.value();
  if (result.is_error()) {
    return result.error_value();
  }

  std::unique_ptr<ramdisk_client> client;
  if (zx_status_t status =
          ramdisk_client::Create(dev_root_fd, result->name.get(), kDeviceWaitTime, &client);
      status != ZX_OK) {
    return status;
  }
  *out = client.release();
  return ZX_OK;
}

__EXPORT
zx_handle_t ramdisk_get_block_interface(const ramdisk_client_t* client) {
  return client->block_interface().channel()->get();
}

__EXPORT
const char* ramdisk_get_path(const ramdisk_client_t* client) { return client->path().c_str(); }

__EXPORT
zx_status_t ramdisk_sleep_after(const ramdisk_client* client, uint64_t block_count) {
  const fidl::WireResult result =
      fidl::WireCall(client->ramdisk_interface())->SleepAfter(block_count);
  if (!result.ok()) {
    return result.status();
  }
  return ZX_OK;
}

__EXPORT
zx_status_t ramdisk_wake(const ramdisk_client* client) {
  const fidl::WireResult result = fidl::WireCall(client->ramdisk_interface())->Wake();
  if (!result.ok()) {
    return result.status();
  }
  return ZX_OK;
}

__EXPORT
zx_status_t ramdisk_grow(const ramdisk_client* client, uint64_t required_size) {
  const fidl::WireResult result = fidl::WireCall(client->ramdisk_interface())->Grow(required_size);
  if (!result.ok()) {
    return result.status();
  }
  return ZX_OK;
}

__EXPORT
zx_status_t ramdisk_set_flags(const ramdisk_client* client, uint32_t flags) {
  const fidl::WireResult result =
      fidl::WireCall(client->ramdisk_interface())
          ->SetFlags(static_cast<fuchsia_hardware_ramdisk::wire::RamdiskFlag>(flags));
  if (!result.ok()) {
    return result.status();
  }
  return ZX_OK;
}

__EXPORT
zx_status_t ramdisk_get_block_counts(const ramdisk_client* client,
                                     ramdisk_block_write_counts_t* out_counts) {
  static_assert(sizeof(ramdisk_block_write_counts_t) ==
                    sizeof(fuchsia_hardware_ramdisk::wire::BlockWriteCounts),
                "Cannot convert between C library / FIDL block counts");

  const fidl::WireResult result = fidl::WireCall(client->ramdisk_interface())->GetBlockCounts();
  if (!result.ok()) {
    return result.status();
  }
  const fidl::WireResponse response = result.value();
  memcpy(out_counts, &response.counts, sizeof(ramdisk_block_write_counts_t));
  return ZX_OK;
}

__EXPORT
zx_status_t ramdisk_rebind(ramdisk_client_t* client) { return client->Rebind(); }

__EXPORT
zx_status_t ramdisk_destroy(ramdisk_client* client) {
  zx_status_t status = client->Destroy();
  delete client;
  return status;
}

__EXPORT
zx_status_t ramdisk_forget(ramdisk_client* client) {
  zx_status_t status = client->Forget();
  delete client;
  return status;
}
