// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <getopt.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/fdio/fd.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/channel.h>

#include <fbl/unique_fd.h>

#include "fidl/fuchsia.hardware.block.volume/cpp/markers.h"
#include "src/lib/digest/digest.h"
#include "src/storage/lib/block_client/cpp/remote_block_device.h"
#include "src/storage/tools/blobfs-corrupt/corrupt_blob.h"

using block_client::RemoteBlockDevice;

namespace {

constexpr char kUsage[] = R"(
Usage: blobfs-corrupt [ <options>* ]

options: (-d|--device) DEVICE    The path to the block device
         (-m|--merkle) MERKLE    The blob identity to corrupt

Given the path to a blobfs block device and a merkle root, this tool corrupts the data contents
of the blob so that it cannot be read when blobfs is mounted.

)";

zx_status_t Usage() {
  fprintf(stderr, kUsage);
  return ZX_ERR_INVALID_ARGS;
}

zx::result<std::tuple<fidl::ClientEnd<fuchsia_hardware_block_volume::Volume>, BlobCorruptOptions>>
ProcessArgs(int argc, char** argv) {
  char* arg_block_path = nullptr;
  char* arg_merkle = nullptr;

  while (true) {
    static struct option opts[] = {
        {"device", required_argument, nullptr, 'd'},
        {"merkle", required_argument, nullptr, 'm'},
    };

    int opt_index;
    int c = getopt_long(argc, argv, "d:m:", opts, &opt_index);

    if (c < 0) {
      break;
    }

    switch (c) {
      case 'd':
        arg_block_path = optarg;
        break;
      case 'm':
        arg_merkle = optarg;
        break;
      default:
        return zx::error(Usage());
    }
  }

  if (arg_block_path == nullptr) {
    FX_LOGS(ERROR) << "'-d <device_path>' is required";
    return zx::error(Usage());
  }

  if (arg_merkle == nullptr) {
    FX_LOGS(ERROR) << "'-m <merkle>' is required";
    return zx::error(Usage());
  }

  BlobCorruptOptions options;
  if (zx_status_t status = options.merkle.Parse(arg_merkle); status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "invalid merkle root: '" << arg_merkle << "'";
    return zx::error(Usage());
  }

  zx::result result = component::Connect<fuchsia_hardware_block_volume::Volume>(arg_block_path);
  if (result.is_error()) {
    FX_PLOGS(ERROR, result.status_value())
        << "unable to open block device: '" << arg_block_path << "'";
    return zx::error(Usage());
  }
  return zx::ok(std::make_tuple(std::move(result.value()), options));
}

}  // namespace

int main(int argc, char** argv) {
  zx::result result = ProcessArgs(argc, argv);
  if (result.is_error()) {
    return -1;
  }
  auto& [client_end, options] = result.value();

  zx::result device = RemoteBlockDevice::Create(std::move(std::move(client_end)));
  if (device.is_error()) {
    FX_PLOGS(ERROR, device.status_value()) << "Could not initialize block device";
    return -1;
  }

  if (zx_status_t status = CorruptBlob(std::move(device.value()), &options); status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "Could not corrupt the requested blob";
    return -1;
  }
  return 0;
}
