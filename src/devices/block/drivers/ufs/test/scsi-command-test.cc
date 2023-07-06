// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <cstdint>
#include <iostream>
#include <memory>

#include "src/devices/block/drivers/ufs/transfer_request_descriptor.h"
#include "src/devices/block/drivers/ufs/upiu/attributes.h"
#include "src/devices/block/drivers/ufs/upiu/descriptors.h"
#include "src/devices/block/drivers/ufs/upiu/scsi_commands.h"
#include "src/devices/block/drivers/ufs/upiu/upiu_transactions.h"
#include "unit-lib.h"

namespace ufs {

using namespace ufs_mock_device;

using ScsiCommandTest = UfsTest;

TEST_F(ScsiCommandTest, Read10) {
  ASSERT_NO_FATAL_FAILURE(RunInit());

  const uint8_t kTestLun = 0;

  uint32_t block_offset = 0;
  uint16_t block_count = 1;
  uint32_t block_size = kMockBlockSize;

  // Write test data to the mock device
  char buf[kMockBlockSize];
  std::strcpy(buf, "test");
  ASSERT_OK(mock_device_->BufferWrite(kTestLun, buf, block_count, block_offset));

  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(kMockBlockSize, 0, &vmo));
  zx::unowned_vmo unowned_vmo(vmo);
  fzl::VmoMapper mapper;
  zx::pmt pmt;

  auto paddrs =
      MapAndPinVmo(ZX_BTI_PERM_WRITE, unowned_vmo, mapper, pmt, 0, block_count * block_size);
  ASSERT_EQ(paddrs.status_value(), ZX_OK);
  auto upiu = std::make_unique<ScsiRead10Upiu>(block_offset, block_count, block_size, 0, 0);

  auto result = ufs_->QueueScsiCommand(std::move(upiu), kTestLun, paddrs.value(), nullptr);
  ASSERT_EQ(result.status_value(), ZX_OK);

  // Check the read data
  std::cout << "mapper=" << static_cast<char *>(mapper.start()) << ", buf=" << buf << std::endl;
  ASSERT_EQ(memcmp(mapper.start(), buf, kMockBlockSize), 0);

  pmt.unpin();
}

TEST_F(ScsiCommandTest, Write10) {
  ASSERT_NO_FATAL_FAILURE(RunInit());

  const uint8_t kTestLun = 0;

  uint32_t block_offset = 0;
  uint16_t block_count = 1;
  uint32_t block_size = kMockBlockSize;

  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(kMockBlockSize, 0, &vmo));
  zx::unowned_vmo unowned_vmo(vmo);
  fzl::VmoMapper mapper;
  zx::pmt pmt;

  auto paddrs =
      MapAndPinVmo(ZX_BTI_PERM_READ, unowned_vmo, mapper, pmt, 0, block_count * block_size);
  ASSERT_EQ(paddrs.status_value(), ZX_OK);
  auto upiu = std::make_unique<ScsiWrite10Upiu>(block_offset, block_count, block_size, 0, 0);
  std::strcpy(static_cast<char *>(mapper.start()), "test");

  auto result = ufs_->QueueScsiCommand(std::move(upiu), kTestLun, paddrs.value(), nullptr);
  ASSERT_EQ(result.status_value(), ZX_OK);

  // Read test data form the mock device
  char buf[kMockBlockSize];
  ASSERT_OK(mock_device_->BufferRead(kTestLun, buf, block_count, block_offset));

  // Check the written data
  std::cout << "mapper=" << static_cast<char *>(mapper.start()) << ", buf=" << buf << std::endl;
  ASSERT_EQ(memcmp(mapper.start(), buf, kMockBlockSize), 0);

  pmt.unpin();
}

TEST_F(ScsiCommandTest, RequestSense) {
  ASSERT_NO_FATAL_FAILURE(RunInit());

  const uint8_t kTestLun = 0;

  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(kMockBlockSize, 0, &vmo));
  zx::unowned_vmo unowned_vmo(vmo);
  fzl::VmoMapper mapper;
  zx::pmt pmt;

  uint16_t block_count = 1;
  uint32_t block_size = kMockBlockSize;

  auto paddrs =
      MapAndPinVmo(ZX_BTI_PERM_WRITE, unowned_vmo, mapper, pmt, 0, block_count * block_size);
  ASSERT_EQ(paddrs.status_value(), ZX_OK);

  auto upiu = std::make_unique<ScsiRequestSenseUpiu>();
  auto result = ufs_->QueueScsiCommand(std::move(upiu), kTestLun, paddrs.value(), nullptr);
  ASSERT_EQ(result.status_value(), ZX_OK);

  auto *sense_data = reinterpret_cast<scsi::FixedFormatSenseDataHeader *>(mapper.start());
  ASSERT_EQ(sense_data->response_code(), 0x70);
  ASSERT_EQ(sense_data->valid(), 0);
  ASSERT_EQ(sense_data->sense_key(), 0);

  pmt.unpin();
}

TEST_F(ScsiCommandTest, SynchronizeCache10) {
  ASSERT_NO_FATAL_FAILURE(RunInit());

  const uint8_t kTestLun = 0;

  uint32_t block_offset = 0;
  uint16_t block_count = 1;

  auto cache_upiu = std::make_unique<ScsiSynchronizeCache10Upiu>(block_offset, block_count);
  auto result = ufs_->QueueScsiCommand(std::move(cache_upiu), kTestLun, {0, 0}, nullptr);
  ASSERT_EQ(result.status_value(), ZX_OK);
}

TEST(ScsiCommandTest, uint24_t) {
  ASSERT_EQ(sizeof(uint24_t), 3);

  // Little-endian
  uint32_t value_32 = 0x123456;  // MSB = 0x12, LSB = 0x56
  uint24_t *value_24_ptr = reinterpret_cast<uint24_t *>(&value_32);

  // Little-endian
  ASSERT_EQ(value_24_ptr->byte[0], 0x56);  // LSB
  ASSERT_EQ(value_24_ptr->byte[1], 0x34);
  ASSERT_EQ(value_24_ptr->byte[2], 0x12);  // MSB
}

TEST(ScsiCommandTest, htobe24) {
  // Little-endian
  uint32_t unsigned_int_32 = 0x123456;  // MSB = 0x12, LSB = 0x56

  // Big-endian
  uint24_t big_24 = htobe24(unsigned_int_32);
  ASSERT_EQ(big_24.byte[0], 0x12);  // MSB
  ASSERT_EQ(big_24.byte[1], 0x34);
  ASSERT_EQ(big_24.byte[2], 0x56);  // LSB
}

TEST(ScsiCommandTest, betoh24) {
  // Big-endian
  uint24_t big_24 = {0x12, 0x34, 0x56};  // MSB = 0x12, LSB = 0x56

  // Little-endian
  uint32_t unsigned_int_32 = betoh24(big_24);
  ASSERT_EQ(unsigned_int_32, 0x123456);  // MSB = 0x12, LSB = 0x56
}

}  // namespace ufs
