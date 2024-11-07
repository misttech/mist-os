// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/virtualization/bin/termina_guest_manager/block_devices.h"

#include <dirent.h>
#include <fcntl.h>
#include <fidl/fuchsia.io/cpp/wire.h>
#include <lib/fdio/cpp/caller.h>
#include <lib/fdio/directory.h>
#include <lib/fdio/fd.h>
#include <lib/fdio/fdio.h>
#include <lib/fdio/namespace.h>
#include <lib/fit/defer.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/trace/event.h>
#include <zircon/hw/gpt.h>

#include <filesystem>

#include <fbl/unique_fd.h>

#include "src/lib/fxl/strings/string_printf.h"

namespace {

namespace fio = fuchsia_io;

// Information about a disk image.
struct DiskImage {
  const char* path;  // Path to the file containing the image
  bool read_only;
  bool create_file;
};

#if defined(USE_VOLATILE_BLOCK)
constexpr bool kForceVolatileWrites = true;
#else
constexpr bool kForceVolatileWrites = false;
#endif

constexpr DiskImage kBlockFileStatefulImage = DiskImage{
    // NOTE: This assumes the /data directory is using Fxfs
    .path = "/data/fxfs_virtualization_guest_image",
    .read_only = false,
    .create_file = true,
};
constexpr DiskImage kFileStatefulImage = DiskImage{
    .path = "/data/fxfs_virtualization_guest_image",
    .read_only = false,
    .create_file = true,
};

constexpr DiskImage kExtrasImage = DiskImage{
    .path = "/pkg/data/termina_extras.img",
    .read_only = true,
    .create_file = false,
};

// Opens the given disk image.
zx::result<fuchsia::io::FileHandle> GetPartition(const DiskImage& image) {
  TRACE_DURATION("termina_guest_manager", "GetPartition");
  fuchsia::io::Flags flags = fuchsia::io::PERM_READABLE;
  if (!image.read_only) {
    flags |= fuchsia::io::PERM_WRITABLE;
  }
  if (image.create_file) {
    flags |= fuchsia::io::Flags::FLAG_MUST_CREATE;
  }
  fuchsia::io::FileHandle file;
  zx_status_t status = fdio_open3(image.path, static_cast<uint64_t>(flags),
                                  file.NewRequest().TakeChannel().release());
  if (status) {
    return zx::error(status);
  }
  return zx::ok(std::move(file));
}

// Opens the given disk image.
zx::result<fidl::InterfaceHandle<fuchsia::hardware::block::Block>> GetFxfsPartition(
    const DiskImage& image, const size_t image_size_bytes) {
  TRACE_DURATION("linux_runner", "GetFxfsPartition");

  // First, use regular file operations to make a huge file at image.path
  // NOTE: image.path is assumed to be a path on an Fxfs filesystem
  fbl::unique_fd fd(open(image.path, O_CREAT | O_RDWR, S_IRUSR | S_IWUSR));
  if (!fd) {
    FX_LOGS(ERROR) << "open(image.path) failed with errno: " << strerror(errno);
    return zx::error(ZX_ERR_IO);
  }

  // Make sure the file is the requested size (image_size_bytes).
  // NOTE: This is usually a huge size (e.g. 40 gigabytes).
  off_t existingFilesize = lseek(fd.get(), 0, SEEK_END);
  if (existingFilesize == static_cast<off_t>(-1) ||
      static_cast<size_t>(existingFilesize) < image_size_bytes) {
    if (ftruncate(fd.get(), image_size_bytes) == -1) {
      FX_LOGS(ERROR) << "ftruncate(image.path) failed with errno: " << strerror(errno);
      return zx::error(ZX_ERR_IO);
    }
  }
  if (close(fd.release()) == -1) {
    FX_LOGS(ERROR) << "close(image.path) failed with errno: " << strerror(errno);
    return zx::error(ZX_ERR_IO);
  }

  /// Now we can try to reopen the file, but in block device mode

  /// First we have to open the parent directory...
  auto dir_endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
  if (dir_endpoints.status_value() != ZX_OK) {
    FX_PLOGS(ERROR, dir_endpoints.status_value())
        << "CreateEndpoints() for Fxfs parent directory failed";
    return zx::error(dir_endpoints.status_value());
  }
  auto [dir_client, dir_server] = *std::move(dir_endpoints);
  std::filesystem::path image_path(image.path);
  uint64_t dir_flags = static_cast<uint64_t>(fio::wire::kPermReadable | fio::wire::kPermWritable |
                                             fio::Flags::kProtocolDirectory);
  zx_status_t dir_open_status =
      fdio_open3(image_path.parent_path().c_str(), dir_flags, dir_server.TakeChannel().release());
  if (dir_open_status != ZX_OK) {
    FX_PLOGS(ERROR, dir_open_status) << "fdio_open3(Fxfs image.path.parent) failed";
    return zx::error(dir_open_status);
  }

  // We want to open the "file" at image.path, but as a block device (i.e. fuchsia.hardware.block).
  fio::OpenFlags flags = fio::OpenFlags::kRightReadable;
  if (!image.read_only) {
    flags |= fio::OpenFlags::kRightWritable;
  }
  auto device_endpoints = fidl::CreateEndpoints<fuchsia_io::Node>();
  if (device_endpoints.status_value() != ZX_OK) {
    FX_PLOGS(ERROR, device_endpoints.status_value())
        << "CreateEndpoints() for Fxfs block device file failed";
    return zx::error(device_endpoints.status_value());
  }
  auto [device_client, device_server] = *std::move(device_endpoints);
  flags |= fio::OpenFlags::kBlockDevice;
  // TODO(https://fxbug.dev/42054266): Consider using io2 for the Open() call.
  auto device_open_result =
      fidl::WireCall(dir_client)
          ->Open(flags, {}, fidl::StringView::FromExternal(image_path.filename().c_str()),
                 std::move(device_server));
  if (!device_open_result.ok()) {
    FX_PLOGS(ERROR, device_open_result.status())
        << "WireCall->Open(image.path) as Fxfs block device failed";
    return zx::error(device_open_result.status());
  }

  return zx::ok(fuchsia::hardware::block::BlockHandle(device_client.TakeChannel()));
}

}  // namespace

fit::result<std::string, std::vector<fuchsia::virtualization::BlockSpec>> GetBlockDevices(
    const termina_config::Config& structured_config, size_t min_size) {
  TRACE_DURATION("termina_guest_manager", "Guest::GetBlockDevices");

  std::vector<fuchsia::virtualization::BlockSpec> devices;

  const uint64_t stateful_image_size_bytes = structured_config.stateful_partition_size();

  // Get/create the stateful partition.
  fuchsia::virtualization::BlockSpec stateful_spec;
  stateful_spec.id = "stateful";
  FX_LOGS(INFO) << "Adding stateful partition type: "
                << structured_config.stateful_partition_type();
  if (structured_config.stateful_partition_type() == "block-file") {
    // Use a file opened with OpenFlags.BLOCK_DEVICE.
    auto handle = GetFxfsPartition(kBlockFileStatefulImage, stateful_image_size_bytes);
    if (handle.is_error()) {
      return fit::error(
          fxl::StringPrintf("Failed to open or create stateful Fxfs file / block device: %s",
                            zx_status_get_string(handle.error_value())));
    }
    stateful_spec.mode = fuchsia::virtualization::BlockMode::READ_WRITE;
    stateful_spec.format.set_block(std::move(handle.value()));
  } else if (structured_config.stateful_partition_type() == "file") {
    // Simple files.
    auto handle = GetPartition(kFileStatefulImage);
    if (handle.is_error()) {
      return fit::error(fxl::StringPrintf("Failed to open or create stateful file: %s",
                                          zx_status_get_string(handle.error_value())));
    }

    auto ptr = handle->BindSync();
    fuchsia::io::File_Resize_Result resize_result;
    zx_status_t status = ptr->Resize(stateful_image_size_bytes, &resize_result);
    if (status != ZX_OK || resize_result.is_err()) {
      return fit::error(fxl::StringPrintf("Failed resize stateful file: %s/%s",
                                          zx_status_get_string(status),
                                          zx_status_get_string(resize_result.err())));
    }

    stateful_spec.mode = fuchsia::virtualization::BlockMode::READ_WRITE;
    stateful_spec.format.set_file(ptr.Unbind());
  } else {
    return fit::error(fxl::StringPrintf("Unsupported partition type: %s",
                                        structured_config.stateful_partition_type().c_str()));
  }
  if (kForceVolatileWrites) {
    stateful_spec.mode = fuchsia::virtualization::BlockMode::VOLATILE_WRITE;
  }
  devices.push_back(std::move(stateful_spec));

  // Add the extras partition if it exists.
  auto extras = GetPartition(kExtrasImage);
  if (extras.is_ok()) {
    devices.push_back({
        .id = "extras",
        .mode = fuchsia::virtualization::BlockMode::VOLATILE_WRITE,
        .format = fuchsia::virtualization::BlockFormat::WithFile(std::move(extras.value())),
    });
  }

  return fit::success(std::move(devices));
}

void DropDevNamespace() {
  // Drop access to /dev, in order to prevent any further access.
  fdio_ns_t* ns;
  zx_status_t status = fdio_ns_get_installed(&ns);
  FX_CHECK(status == ZX_OK) << "Failed to get installed namespace";
  if (fdio_ns_is_bound(ns, "/dev")) {
    status = fdio_ns_unbind(ns, "/dev");
    FX_CHECK(status == ZX_OK) << "Failed to unbind '/dev' from the installed namespace";
  }
}
