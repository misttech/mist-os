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

class ScsiCommandTest : public UfsTest {
 public:
  void SetUp() override {
    UfsTest::SetUp();
    ASSERT_NO_FATAL_FAILURE(RunInit());

    // Create a mapped and pinned vmo.
    ASSERT_OK(zx::vmo::create(kMockBlockSize, 0, &vmo_));
    zx::unowned_vmo unowned_vmo(vmo_);

    auto paddrs_or =
        MapAndPinVmo(ZX_BTI_PERM_WRITE, unowned_vmo, mapper_, pmt_, 0, block_count_ * block_size_);
    ASSERT_OK(paddrs_or);
    paddrs_ = paddrs_or.value();
  }

  void TearDown() override {
    pmt_.unpin();
    UfsTest::TearDown();
  }

  void *GetVirtualAddress() const { return mapper_.start(); }
  std::array<zx_paddr_t, 2> GetPhysicalAddress() const { return paddrs_; }

  uint16_t GetBlockCount() const { return block_count_; }
  uint32_t GetBlockSize() const { return block_size_; }

 private:
  zx::vmo vmo_;
  zx::pmt pmt_;
  fzl::VmoMapper mapper_;
  std::array<zx_paddr_t, 2> paddrs_;

  const uint16_t block_count_ = 1;
  const uint32_t block_size_ = kMockBlockSize;
};

TEST_F(ScsiCommandTest, Read10) {
  const uint8_t kTestLun = 0;
  uint32_t block_offset = 0;

  // Write test data to the mock device
  char buf[kMockBlockSize];
  constexpr char kTestString[] = "test";
  std::strncpy(buf, kTestString, sizeof(buf));
  ASSERT_OK(mock_device_->BufferWrite(kTestLun, buf, GetBlockCount(), block_offset));

  auto upiu = std::make_unique<ScsiRead10Upiu>(block_offset, GetBlockCount(), GetBlockSize(), 0, 0);
  auto result = ufs_->QueueScsiCommand(std::move(upiu), kTestLun, GetPhysicalAddress(), nullptr);
  ASSERT_EQ(result.status_value(), ZX_OK);

  // Check the read data
  ASSERT_EQ(memcmp(GetVirtualAddress(), buf, kMockBlockSize), 0);
}

TEST_F(ScsiCommandTest, Write10) {
  const uint8_t kTestLun = 0;
  uint32_t block_offset = 0;

  auto upiu =
      std::make_unique<ScsiWrite10Upiu>(block_offset, GetBlockCount(), GetBlockSize(), 0, 0);
  constexpr char kTestString[] = "test";
  std::strncpy(static_cast<char *>(GetVirtualAddress()), kTestString, kMockBlockSize);

  auto result = ufs_->QueueScsiCommand(std::move(upiu), kTestLun, GetPhysicalAddress(), nullptr);
  ASSERT_EQ(result.status_value(), ZX_OK);

  // Read test data form the mock device
  char buf[kMockBlockSize];
  ASSERT_OK(mock_device_->BufferRead(kTestLun, buf, GetBlockCount(), block_offset));

  // Check the written data
  ASSERT_EQ(memcmp(GetVirtualAddress(), buf, kMockBlockSize), 0);
}

TEST_F(ScsiCommandTest, ReadCapacity10) {
  const uint8_t kTestLun = 0;

  auto upiu = std::make_unique<ScsiReadCapacity10Upiu>();
  auto result = ufs_->QueueScsiCommand(std::move(upiu), kTestLun, GetPhysicalAddress(), nullptr);
  ASSERT_EQ(result.status_value(), ZX_OK);

  auto *read_capacity_data =
      reinterpret_cast<scsi::ReadCapacity10ParameterData *>(GetVirtualAddress());

  // |returned_logical_block_address| is a 0-based value.
  ASSERT_EQ(betoh32(read_capacity_data->returned_logical_block_address),
            (kMockTotalDeviceCapacity / kMockBlockSize) - 1);
  ASSERT_EQ(betoh32(read_capacity_data->block_length_in_bytes), kMockBlockSize);
}

TEST_F(ScsiCommandTest, RequestSense) {
  const uint8_t kTestLun = 0;

  auto upiu = std::make_unique<ScsiRequestSenseUpiu>();
  auto result = ufs_->QueueScsiCommand(std::move(upiu), kTestLun, GetPhysicalAddress(), nullptr);
  ASSERT_EQ(result.status_value(), ZX_OK);

  auto *sense_data = reinterpret_cast<scsi::FixedFormatSenseDataHeader *>(GetVirtualAddress());
  ASSERT_EQ(sense_data->response_code(), 0x70);
  ASSERT_EQ(sense_data->valid(), 0);
  ASSERT_EQ(sense_data->sense_key(), 0);
}

TEST_F(ScsiCommandTest, SynchronizeCache10) {
  const uint8_t kTestLun = 0;
  uint32_t block_offset = 0;

  auto cache_upiu = std::make_unique<ScsiSynchronizeCache10Upiu>(block_offset, GetBlockCount());
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
