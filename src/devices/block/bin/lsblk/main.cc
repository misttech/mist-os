// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <dirent.h>
#include <fidl/fuchsia.device/cpp/wire.h>
#include <fidl/fuchsia.hardware.block.partition/cpp/wire.h>
#include <fidl/fuchsia.hardware.block/cpp/wire.h>
#include <fidl/fuchsia.hardware.skipblock/cpp/wire.h>
#include <inttypes.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/fit/defer.h>
#include <lib/fzl/owned-vmo-mapper.h>
#include <lib/zx/vmo.h>
#include <limits.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>
#include <zircon/process.h>
#include <zircon/status.h>
#include <zircon/syscalls.h>
#include <zircon/types.h>

#include <memory>
#include <string>

#include <fbl/unique_fd.h>
#include <gpt/c/gpt.h>
#include <gpt/guid.h>
#include <pretty/hexdump.h>

#include "src/lib/fxl/strings/string_printf.h"
#include "src/storage/lib/storage-metrics/block-metrics.h"

namespace fuchsia_block = fuchsia_hardware_block;
namespace fuchsia_partition = fuchsia_hardware_block_partition;
namespace fuchsia_skipblock = fuchsia_hardware_skipblock;

namespace {

constexpr char DEV_BLOCK[] = "/dev/class/block";
constexpr char DEV_SKIP_BLOCK[] = "/dev/class/skip-block";

char* size_to_cstring(char* str, size_t maxlen, uint64_t size) {
  constexpr size_t kibi = 1024;
  const char* unit;
  uint64_t div;
  if (size < kibi) {
    unit = "";
    div = 1;
  } else if (size >= kibi && size < kibi * kibi) {
    unit = "K";
    div = kibi;
  } else if (size >= kibi * kibi && size < kibi * kibi * kibi) {
    unit = "M";
    div = kibi * kibi;
  } else if (size >= kibi * kibi * kibi && size < kibi * kibi * kibi * kibi) {
    unit = "G";
    div = kibi * kibi * kibi;
  } else {
    unit = "T";
    div = kibi * kibi * kibi * kibi;
  }
  snprintf(str, maxlen, "%" PRIu64 "%s", size / div, unit);
  return str;
}

int cmd_list_blk() {
  struct dirent* de;
  DIR* dir = opendir(DEV_BLOCK);
  if (!dir) {
    fprintf(stderr, "Error opening %s\n", DEV_BLOCK);
    return -1;
  }
  auto cleanup = fit::defer([&dir]() { closedir(dir); });

  printf("%-3s %-4s %-16s %-20s %-6s %s\n", "ID", "SIZE", "TYPE", "LABEL", "FLAGS", "DEVICE");

  while ((de = readdir(dir)) != nullptr) {
    if (!strcmp(de->d_name, ".") || !strcmp(de->d_name, "..")) {
      continue;
    }
    std::string device_path = fxl::StringPrintf("%s/%s", DEV_BLOCK, de->d_name);
    std::string controller_path = device_path + "/device_controller";

    std::string topological_path;
    {
      zx::result controller = component::Connect<fuchsia_device::Controller>(controller_path);
      if (controller.is_error()) {
        fprintf(stderr, "Error opening %s: %s\n", controller_path.c_str(),
                controller.status_string());
        continue;
      }
      fidl::WireResult result = fidl::WireCall(controller.value())->GetTopologicalPath();
      if (!result.ok()) {
        fprintf(stderr, "Error getting topological path for %s: %s\n", controller_path.c_str(),
                result.status_string());
        continue;
      }
      fit::result response = result.value();
      if (response.is_error()) {
        fprintf(stderr, "Error getting topological path for %s: %s\n", controller_path.c_str(),
                zx_status_get_string(response.error_value()));
        continue;
      }
      topological_path = response.value()->path.get();
    }

    zx::result device = component::Connect<fuchsia_partition::Partition>(device_path);
    if (device.is_error()) {
      fprintf(stderr, "Error opening %s: %s\n", device_path.c_str(), device.status_string());
      continue;
    }

    char sizestr[6] = {};
    fuchsia_block::wire::BlockInfo block_info;
    {
      if (const fidl::WireResult result = fidl::WireCall(device.value())->GetInfo(); result.ok()) {
        if (const fit::result response = result.value(); response.is_ok()) {
          block_info = response.value()->info;
          size_to_cstring(sizestr, sizeof(sizestr), block_info.block_size * block_info.block_count);
        }
      }
    }

    std::string type;
    std::string label;
    {
      if (const fidl::WireResult result = fidl::WireCall(device.value())->GetTypeGuid();
          result.ok()) {
        if (const fidl::WireResponse response = result.value(); response.status == ZX_OK) {
          type = gpt::KnownGuid::TypeDescription(response.guid->value.data());
        }
      }
      if (const fidl::WireResult result = fidl::WireCall(device.value())->GetName(); result.ok()) {
        if (const fidl::WireResponse response = result.value(); response.status == ZX_OK) {
          label = response.name.get();
        }
      }
    }

    char flags[20] = {0};
    if (block_info.flags & fuchsia_block::wire::Flag::kReadonly) {
      strlcat(flags, "RO ", sizeof(flags));
    }
    if (block_info.flags & fuchsia_block::wire::Flag::kRemovable) {
      strlcat(flags, "RE ", sizeof(flags));
    }
    if (block_info.flags & fuchsia_block::wire::Flag::kBootpart) {
      strlcat(flags, "BP ", sizeof(flags));
    }
    printf("%-3s %4s %-16s %-20s %-6s %s\n", de->d_name, sizestr, type.c_str(), label.c_str(),
           flags, topological_path.c_str());
  }
  return 0;
}

int cmd_list_skip_blk() {
  struct dirent* de;
  DIR* dir = opendir(DEV_SKIP_BLOCK);
  if (!dir) {
    fprintf(stderr, "Error opening %s\n", DEV_SKIP_BLOCK);
    return -1;
  }
  while ((de = readdir(dir)) != nullptr) {
    if (!strcmp(de->d_name, ".") || !strcmp(de->d_name, "..")) {
      continue;
    }
    std::string device_path = fxl::StringPrintf("%s/%s", DEV_SKIP_BLOCK, de->d_name);
    std::string controller_path = device_path + "/device_controller";

    std::string topological_path;
    {
      zx::result controller = component::Connect<fuchsia_device::Controller>(controller_path);
      if (controller.is_error()) {
        fprintf(stderr, "Error opening %s: %s\n", controller_path.c_str(),
                controller.status_string());
        continue;
      }
      fidl::WireResult result = fidl::WireCall(controller.value())->GetTopologicalPath();
      if (!result.ok()) {
        fprintf(stderr, "Error getting topological path for %s: %s\n", controller_path.c_str(),
                result.status_string());
        continue;
      }
      fit::result response = result.value();
      if (response.is_error()) {
        fprintf(stderr, "Error getting topological path for %s: %s\n", controller_path.c_str(),
                zx_status_get_string(response.error_value()));
        continue;
      }
      topological_path = response.value()->path.get();
    }

    std::string type;
    {
      zx::result device = component::Connect<fuchsia_skipblock::SkipBlock>(device_path);
      if (device.is_error()) {
        fprintf(stderr, "Error opening %s: %s\n", device_path.c_str(), device.status_string());
        continue;
      }
      if (const fidl::WireResult result = fidl::WireCall(device.value())->GetPartitionInfo();
          result.ok()) {
        if (const fidl::WireResponse response = result.value(); response.status == ZX_OK) {
          type = gpt::KnownGuid::TypeDescription(response.partition_info.partition_guid.data());
        }
      }
    }

    printf("%-3s %4sr%-16s %-20s %-6s %s\n", de->d_name, "", type.c_str(), "", "",
           topological_path.c_str());
  }
  closedir(dir);
  return 0;
}

int try_read_skip_blk(const fidl::UnownedClientEnd<fuchsia_skipblock::SkipBlock>& skip_block,
                      off_t offset, size_t count) {
  // check that count and offset are aligned to block size
  const fidl::WireResult result = fidl::WireCall(skip_block)->GetPartitionInfo();
  if (!result.ok()) {
    fprintf(stderr, "Failed to get skip block partition info: %s\n",
            result.FormatDescription().c_str());
    return -1;
  }
  const fidl::WireResponse response = result.value();
  if (zx_status_t status = response.status; status != ZX_OK) {
    fprintf(stderr, "Failed to get skip block partition info: %s\n", zx_status_get_string(status));
    return -1;
  }
  uint64_t blksize = response.partition_info.block_size_bytes;
  if (count % blksize) {
    fprintf(stderr, "Bytes read must be a multiple of blksize=%" PRIu64 "\n", blksize);
    return -1;
  }
  if (offset % blksize) {
    fprintf(stderr, "Offset must be a multiple of blksize=%" PRIu64 "\n", blksize);
    return -1;
  }

  // allocate and map a buffer to read into
  zx::vmo vmo;
  if (zx_status_t status = zx::vmo::create(count, 0, &vmo); status != ZX_OK) {
    fprintf(stderr, "Failed to create vmo: %s\n", zx_status_get_string(status));
    return -1;
  }

  fzl::OwnedVmoMapper mapper;
  if (zx_status_t status = mapper.Map(std::move(vmo), count); status != ZX_OK) {
    fprintf(stderr, "Failed to map vmo: %s\n", zx_status_get_string(status));
    return -1;
  }
  zx::vmo dup;
  if (zx_status_t status = mapper.vmo().duplicate(ZX_RIGHT_SAME_RIGHTS, &dup); status != ZX_OK) {
    fprintf(stderr, "Failed duplicate handle: %s\n", zx_status_get_string(status));
    return -1;
  }

  // read the data
  const fidl::WireResult read_result =
      fidl::WireCall(skip_block)
          ->Read({
              .vmo = std::move(dup),
              .vmo_offset = 0,
              .block = static_cast<uint32_t>(offset / blksize),
              .block_count = static_cast<uint32_t>(count / blksize),
          });
  if (!read_result.ok()) {
    fprintf(stderr, "Failed to read skip block: %s\n", read_result.FormatDescription().c_str());
    return -1;
  }
  const fidl::WireResponse read_response = read_result.value();
  if (zx_status_t status = read_response.status; status != ZX_OK) {
    fprintf(stderr, "Failed to read skip block: %s\n", zx_status_get_string(status));
    return -1;
  }

  hexdump8_ex(mapper.start(), count, offset);
  return 0;
}

int cmd_read_blk(const char* dev, off_t offset, size_t count) {
  zx::result block = component::Connect<fuchsia_block::Block>(dev);
  if (block.is_error()) {
    fprintf(stderr, "Error connecting to %s: %s\n", dev, block.status_string());
    return -1;
  }

  const fidl::WireResult result = fidl::WireCall(block.value())->GetInfo();
  if (!result.ok()) {
    fprintf(stderr, "Error getting block size for %s: %s\n", dev,
            result.FormatDescription().c_str());
    return -1;
  }
  const fit::result response = result.value();
  if (response.is_error()) {
    zx::result skip_block = component::Connect<fuchsia_skipblock::SkipBlock>(dev);
    if (skip_block.is_error()) {
      fprintf(stderr, "Error connecting to %s: %s\n", dev, skip_block.status_string());
      return -1;
    }
    if (try_read_skip_blk(skip_block.value(), offset, count) < 0) {
      fprintf(stderr, "Error getting block size for %s\n", dev);
      return -1;
    }
    return 0;
  }
  // Check that count and offset are aligned to block size.
  uint64_t blksize = response.value()->info.block_size;
  if (count % blksize) {
    fprintf(stderr, "Bytes read must be a multiple of blksize=%" PRIu64 "\n", blksize);
    return -1;
  }
  if (offset % blksize) {
    fprintf(stderr, "Offset must be a multiple of blksize=%" PRIu64 "\n", blksize);
    return -1;
  }

  // Create vmo for reading, and handle clone for passing off.
  zx::vmo in_vmo;
  if (zx_status_t status = zx::vmo::create(count, 0u, &in_vmo); status != ZX_OK) {
    fprintf(stderr, "Failed to create read vmo: %s.\n", zx_status_get_string(status));
    return -1;
  }
  zx::vmo out_vmo;
  if (zx_status_t status = in_vmo.duplicate(ZX_RIGHTS_BASIC, &out_vmo); status != ZX_OK) {
    fprintf(stderr, "Failed to duplicate vmo handle: %s.\n", zx_status_get_string(status));
    return -1;
  }
  fzl::OwnedVmoMapper mapper;
  if (zx_status_t status = mapper.Map(std::move(in_vmo), count); status != ZX_OK) {
    fprintf(stderr, "Failed to map vmo: %s\n", zx_status_get_string(status));
    return -1;
  }

  // read the data
  const fidl::WireResult read_result =
      fidl::WireCall(block.value())->ReadBlocks(std::move(out_vmo), count, offset, 0);
  if (!read_result.ok()) {
    fprintf(stderr, "Error from block device read: %s\n", read_result.FormatDescription().c_str());
    return -1;
  }

  hexdump8_ex(mapper.start(), count, offset);
  return 0;
}

int cmd_stats(const char* dev, bool clear) {
  zx::result block = component::Connect<fuchsia_block::Block>(dev);
  if (block.is_error()) {
    fprintf(stderr, "Error connecting to %s: %s\n", dev, block.status_string());
    return -1;
  }
  const fidl::WireResult result = fidl::WireCall(block.value())->GetStats(clear);
  if (!result.ok()) {
    fprintf(stderr, "Error getting stats for %s: %s\n", dev, result.FormatDescription().c_str());
    return -1;
  }
  const fit::result response = result.value();
  if (response.is_error()) {
    fprintf(stderr, "Error getting stats for %s: %s\n", dev,
            zx_status_get_string(response.error_value()));
    return -1;
  }
  storage_metrics::BlockDeviceMetrics metrics(&response.value()->stats);
  metrics.Dump(stdout);
  return 0;
}

}  // namespace

int main(int argc, const char** argv) {
  int rc = 0;
  const char* cmd = argc > 1 ? argv[1] : nullptr;
  if (cmd) {
    if (!strcmp(cmd, "help")) {
      goto usage;
    } else if (!strcmp(cmd, "read")) {
      if (argc < 5)
        goto usage;
      rc = cmd_read_blk(argv[2], strtoul(argv[3], nullptr, 10), strtoull(argv[4], nullptr, 10));
    } else if (!strcmp(cmd, "stats")) {
      if (argc < 4)
        goto usage;
      if (strcmp("true", argv[3]) != 0 && strcmp("false", argv[3]) != 0)
        goto usage;
      rc = cmd_stats(argv[2], strcmp("true", argv[3]) == 0);
    } else {
      fprintf(stderr, "Unrecognized command %s!\n", cmd);
      goto usage;
    }
  } else {
    rc = cmd_list_blk() || cmd_list_skip_blk();
  }
  return rc;
usage:
  fprintf(stderr, "Usage:\n");
  fprintf(stderr, "%s\n", argv[0]);
  fprintf(stderr, "%s read <blkdev> <offset> <count>\n", argv[0]);
  fprintf(stderr, "%s stats <blkdev> <clear=true|false>\n", argv[0]);
  return 0;
}
