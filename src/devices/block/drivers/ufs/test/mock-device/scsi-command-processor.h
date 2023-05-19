// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BLOCK_DRIVERS_UFS_TEST_MOCK_DEVICE_SCSI_COMMAND_PROCESSOR_H_
#define SRC_DEVICES_BLOCK_DRIVERS_UFS_TEST_MOCK_DEVICE_SCSI_COMMAND_PROCESSOR_H_

#include <lib/mmio-ptr/fake.h>
#include <lib/mmio/mmio-buffer.h>
#include <lib/stdcompat/span.h>

#include <functional>
#include <vector>

#include "handler.h"
#include "src/devices/block/drivers/ufs/ufs.h"
#include "src/devices/block/drivers/ufs/upiu/scsi_commands.h"

namespace ufs {
namespace ufs_mock_device {

class UfsMockDevice;

class ScsiCommandProcessor {
 public:
  using ScsiCommandHandler = std::function<zx::result<std::vector<uint8_t>>(
      UfsMockDevice &, CommandUpiu::Data &, ResponseUpiu::Data &,
      cpp20::span<PhysicalRegionDescriptionTableEntry> &)>;

  ScsiCommandProcessor(const ScsiCommandProcessor &) = delete;
  ScsiCommandProcessor &operator=(const ScsiCommandProcessor &) = delete;
  ScsiCommandProcessor(const ScsiCommandProcessor &&) = delete;
  ScsiCommandProcessor &operator=(const ScsiCommandProcessor &&) = delete;
  ~ScsiCommandProcessor() = default;
  explicit ScsiCommandProcessor(UfsMockDevice &mock_device) : mock_device_(mock_device) {}
  zx_status_t HandleScsiCommand(CommandUpiu::Data &command_upiu, ResponseUpiu::Data &response_upiu,
                                cpp20::span<PhysicalRegionDescriptionTableEntry> &prdt_upiu);

  static zx::result<std::vector<uint8_t>> DefaultRequestSenseHandler(
      UfsMockDevice &mock_device, CommandUpiu::Data &command_upiu,
      ResponseUpiu::Data &response_upiu,
      cpp20::span<PhysicalRegionDescriptionTableEntry> &prdt_upius);

  static zx::result<std::vector<uint8_t>> DefaultRead10Handler(
      UfsMockDevice &mock_device, CommandUpiu::Data &command_upiu,
      ResponseUpiu::Data &response_upiu,
      cpp20::span<PhysicalRegionDescriptionTableEntry> &prdt_upius);
  static zx::result<std::vector<uint8_t>> DefaultWrite10Handler(
      UfsMockDevice &mock_device, CommandUpiu::Data &command_upiu,
      ResponseUpiu::Data &response_upiu,
      cpp20::span<PhysicalRegionDescriptionTableEntry> &prdt_upius);
  static zx::result<std::vector<uint8_t>> DefaultSynchronizeCache10Handler(
      UfsMockDevice &mock_device, CommandUpiu::Data &command_upiu,
      ResponseUpiu::Data &response_upiu,
      cpp20::span<PhysicalRegionDescriptionTableEntry> &prdt_upius);

  DEF_DEFAULT_HANDLER_BEGIN(ScsiOpcode, ScsiCommandHandler)
  DEF_DEFAULT_HANDLER(ScsiOpcode::kRequestSense, DefaultRequestSenseHandler)
  DEF_DEFAULT_HANDLER(ScsiOpcode::kRead10, DefaultRead10Handler)
  DEF_DEFAULT_HANDLER(ScsiOpcode::kWrite10, DefaultWrite10Handler)
  DEF_DEFAULT_HANDLER(ScsiOpcode::kSynchronizeCache10, DefaultSynchronizeCache10Handler)
  DEF_DEFAULT_HANDLER_END()

 private:
  void BuildSenseData(ResponseUpiu::Data &response_upiu, uint8_t sense_key);

  UfsMockDevice &mock_device_;
};

}  // namespace ufs_mock_device
}  // namespace ufs

#endif  // SRC_DEVICES_BLOCK_DRIVERS_UFS_TEST_MOCK_DEVICE_SCSI_COMMAND_PROCESSOR_H_
