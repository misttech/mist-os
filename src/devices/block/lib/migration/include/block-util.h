// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BLOCK_LIB_MIGRATION_INCLUDE_BLOCK_UTIL_H_
#define SRC_DEVICES_BLOCK_LIB_MIGRATION_INCLUDE_BLOCK_UTIL_H_

#include "src/lib/storage/block_client/cpp/block_device.h"
#include "src/lib/storage/block_client/cpp/fake_block_device.h"

namespace block {

// TODO(fxbug.dev/122694): Remove this utility when the block interface change has completed.

inline zx_status_t IssueBlockFifoRequest(const std::string& command, uint16_t vmoid,
                                         uint32_t length, uint64_t vmo_offset, uint64_t dev_offset,
                                         block_client::BlockDevice* device) {
  uint8_t opcode;
  if (command == "READ") {
    opcode = BLOCK_OPCODE_READ;
  } else if (command == "WRITE") {
    opcode = BLOCK_OPCODE_WRITE;
  } else if (command == "FLUSH") {
    opcode = BLOCK_OPCODE_FLUSH;
  } else if (command == "TRIM") {
    opcode = BLOCK_OPCODE_TRIM;
  } else if (command == "CLOSE_VMO") {
    opcode = BLOCK_OPCODE_CLOSE_VMO;
  } else {
    return ZX_ERR_INVALID_ARGS;
  }

  block_fifo_request_t request = {
      .command = {.opcode = opcode, .flags = 0},
      .reqid = 0,
      .group = 0,
      .vmoid = vmoid,
      .length = length,
      .vmo_offset = vmo_offset,
      .dev_offset = dev_offset,
  };

  return device->FifoTransaction(&request, 1);
}

inline void SetFakeBlockDeviceHook(block_client::FakeBlockDevice* device, uint32_t* read_req_count,
                                   uint64_t* blocks_read = nullptr,
                                   uint32_t* write_req_count = nullptr,
                                   uint64_t* blocks_written = nullptr,
                                   uint32_t* flush_req_count = nullptr,
                                   uint32_t* trim_req_count = nullptr) {
  auto hook = [=](const block_fifo_request_t& request, const zx::vmo* vmo) {
    switch (request.command.opcode) {
      case BLOCK_OPCODE_READ:
        if (read_req_count != nullptr)
          (*read_req_count)++;
        if (blocks_read != nullptr)
          (*blocks_read) += request.length;
        break;
      case BLOCK_OPCODE_WRITE:
        if (write_req_count != nullptr)
          (*write_req_count)++;
        if (blocks_written != nullptr)
          (*blocks_written) += request.length;
        break;
      case BLOCK_OPCODE_FLUSH:
        if (flush_req_count != nullptr)
          (*flush_req_count)++;
        break;
      case BLOCK_OPCODE_TRIM:
        if (trim_req_count != nullptr)
          (*trim_req_count)++;
        break;
      default:
        return ZX_ERR_INVALID_ARGS;
    }

    return ZX_OK;
  };

  device->set_hook(hook);
}

}  // namespace block

#endif  // SRC_DEVICES_BLOCK_LIB_MIGRATION_INCLUDE_BLOCK_UTIL_H_
