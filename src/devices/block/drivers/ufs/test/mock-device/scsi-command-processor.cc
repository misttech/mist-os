// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/ddk/debug.h>

#include <algorithm>

#include "ufs-mock-device.h"

namespace ufs {
namespace ufs_mock_device {

zx_status_t ScsiCommandProcessor::CopyBufferToPhysicalRegion(
    cpp20::span<PhysicalRegionDescriptionTableEntry> &prdt_upius,
    const std::vector<uint8_t> &buffer) {
  uint64_t cur_pos = 0;
  for (auto &prdt_upiu : prdt_upius) {
    if (cur_pos >= buffer.size()) {
      break;
    }

    zx_paddr_t data_buffer_paddr =
        (static_cast<zx_paddr_t>(prdt_upiu.data_base_address_upper()) << 32) |
        prdt_upiu.data_base_address();
    uint8_t *data_buffer;
    if (auto result = mock_device_.MapDmaPaddr(data_buffer_paddr); result.is_error()) {
      return result.error_value();
    } else {
      data_buffer = reinterpret_cast<uint8_t *>(*result);
    }

    uint64_t data_buffer_size = prdt_upiu.data_byte_count() + 1;
    uint64_t transfer_count = std::min(data_buffer_size, buffer.size() - cur_pos);
    std::memcpy(data_buffer, buffer.data() + cur_pos, transfer_count);

    cur_pos += transfer_count;
  }

  if (cur_pos != buffer.size()) {
    return ZX_ERR_NO_SPACE;
  }

  return ZX_OK;
}

void ScsiCommandProcessor::BuildSenseData(ResponseUpiu::Data &response_upiu, uint8_t sense_key) {
  response_upiu.header.data_segment_length = htobe16(sizeof(ScsiSenseData));
  response_upiu.sense_data_len = htobe16(sizeof(ScsiSenseData));
  ScsiSenseData *sense_data = reinterpret_cast<ScsiSenseData *>(response_upiu.sense_data);
  sense_data->resp_code = 0x70;
  sense_data->valid = 0;
  sense_data->sense_key = sense_key;
}

zx_status_t ScsiCommandProcessor::HandleScsiCommand(
    CommandUpiu::Data &command_upiu, ResponseUpiu::Data &response_upiu,
    cpp20::span<PhysicalRegionDescriptionTableEntry> &prdt_upius) {
  constexpr uint8_t kIllegalRequest = 0x05;
  ScsiOpcode opcode = static_cast<ScsiOpcode>(command_upiu.cdb[0]);
  std::vector<uint8_t> data;
  if (auto it = handlers_.find(opcode); it != handlers_.end()) {
    if (auto result = (it->second)(mock_device_, command_upiu, response_upiu); result.is_error()) {
      BuildSenseData(response_upiu, kIllegalRequest);
      return result.status_value();
    } else {
      data = std::move(*result);
    }
  } else {
    zxlogf(ERROR, "UFS MOCK: scsi command opcode: 0x%x is not supported", opcode);
    BuildSenseData(response_upiu, kIllegalRequest);
    return ZX_ERR_NOT_SUPPORTED;
  }

  return CopyBufferToPhysicalRegion(prdt_upius, data);
}

zx::result<std::vector<uint8_t>> ScsiCommandProcessor::DefaultRequestSenseHandler(
    UfsMockDevice &mock_device, CommandUpiu::Data &command_upiu,
    ResponseUpiu::Data &response_upiu) {
  std::vector<uint8_t> data_buffer;

  auto request_sense_command = ScsiCommandUpiu::CopyFrom<ScsiRequestSenseUpiu>(&command_upiu);
  if (request_sense_command->desc() != 0) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  data_buffer.resize(sizeof(ScsiSenseData));
  ScsiSenseData *sense_data = reinterpret_cast<ScsiSenseData *>(data_buffer.data());
  sense_data->resp_code = 0x70;
  sense_data->valid = 0;
  sense_data->sense_key = 0;

  return zx::ok(std::move(data_buffer));
}

zx::result<std::vector<uint8_t>> ScsiCommandProcessor::DefaultRead10Handler(
    UfsMockDevice &mock_device, CommandUpiu::Data &command_upiu,
    ResponseUpiu::Data &response_upiu) {
  uint8_t lun = command_upiu.header.lun;
  auto read10_command = ScsiCommandUpiu::CopyFrom<ScsiRead10Upiu>(&command_upiu);

  std::vector<uint8_t> data_buffer(read10_command->GetTransferBytes());
  if (auto status = mock_device.BufferRead(lun, data_buffer.data(),
                                           read10_command->GetTransferBytes() / kMockBlockSize,
                                           read10_command->GetStartLba().value());
      status != ZX_OK) {
    return zx::error(status);
  }

  return zx::ok(std::move(data_buffer));
}

}  // namespace ufs_mock_device
}  // namespace ufs
