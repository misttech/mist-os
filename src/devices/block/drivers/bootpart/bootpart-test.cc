// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "bootpart.h"

#include <lib/stdcompat/span.h>

#include <zxtest/zxtest.h>

#include "src/devices/testing/mock-ddk/mock-device.h"

namespace bootpart {

class FakeBlockDevice : public ddk::BlockImplProtocol<FakeBlockDevice> {
 public:
  FakeBlockDevice() { memset(data_, 0xff, sizeof(data_)); }

  char* data() { return data_; }
  bool flushed() const { return flushed_; }

  const block_impl_protocol_ops_t* BlockOps() const { return &block_impl_protocol_ops_; }

  void BlockImplQuery(block_info_t* out_info, uint64_t* out_block_op_size) {
    out_info->block_count = 24;
    out_info->block_size = 10;
    out_info->max_transfer_size = 10;
    out_info->flags = 0;
    *out_block_op_size = sizeof(block_op_t);
  }

  void BlockImplQueue(block_op_t* txn, block_impl_queue_callback callback, void* cookie) {
    zx_status_t status = ZX_ERR_NOT_SUPPORTED;
    switch (txn->command.opcode) {
      case BLOCK_OPCODE_READ: {
        static uint64_t expected_lba = 0;
        if (txn->rw.length != 1 || txn->rw.offset_dev != expected_lba) {
          status = ZX_ERR_OUT_OF_RANGE;
        } else {
          status = zx_vmo_write(txn->rw.vmo, data_ + txn->rw.offset_dev * 10,
                                txn->rw.offset_vmo * 10, 10);
          expected_lba += 12;  // Expect next read at LBA == 12.
        }
        break;
      }
      case BLOCK_OPCODE_WRITE: {
        static uint64_t expected_lba = 0;
        if (txn->rw.length != 1 || txn->rw.offset_dev != expected_lba) {
          status = ZX_ERR_OUT_OF_RANGE;
        } else {
          status = zx_vmo_read(txn->rw.vmo, data_ + txn->rw.offset_dev * 10,
                               txn->rw.offset_vmo * 10, 10);
          flushed_ = status != ZX_OK && flushed_;
          expected_lba += 12;  // Expect next write at LBA == 12.
        }
        break;
      }
      case BLOCK_OPCODE_FLUSH:
        flushed_ = true;
        status = ZX_OK;
        break;
      default:
        break;
    }

    callback(cookie, status, txn);
  }

 private:
  char data_[24 * 10];  // 24 blocks of 10 bytes each.
  bool flushed_ = true;
};

class BootPartitionTest : public zxtest::Test {
 public:
  void SetUp() override {
    parent_->AddProtocol(ZX_PROTOCOL_BLOCK_IMPL, fake_block_device_.BlockOps(),
                         &fake_block_device_);

    // Set up 2 partitions of equal size.
    std::vector<uint8_t> partition_map_buffer;
    // Add __STDCPP_DEFAULT_NEW_ALIGNMENT__ as loose upper-bound on alignment padding.
    partition_map_buffer.resize(sizeof(zbi_partition_map_t) + __STDCPP_DEFAULT_NEW_ALIGNMENT__ +
                                    2 * sizeof(zbi_partition_t),
                                0);
    zbi_partition_map_t* partition_map =
        reinterpret_cast<zbi_partition_map_t*>(partition_map_buffer.data());
    partition_map->partition_count = 2;

    static_assert(alignof(zbi_partition_map_t) >= alignof(zbi_partition_t));
    cpp20::span<zbi_partition_t> partitions(reinterpret_cast<zbi_partition_t*>(partition_map + 1),
                                            partition_map->partition_count);

    // Set up partition 0.
    partitions[0].first_block = 0;
    partitions[0].last_block = 11;
    memset(partitions[0].type_guid, 'T', sizeof(partitions[0].type_guid));
    memset(partitions[0].uniq_guid, 'I', sizeof(partitions[0].uniq_guid));
    strncpy(partitions[0].name, "This is partition0", sizeof(partitions[0].name));

    // Set up partition 1.
    partitions[1].first_block = 12;
    partitions[1].last_block = 23;
    memset(partitions[1].type_guid, 'U', sizeof(partitions[1].type_guid));
    memset(partitions[1].uniq_guid, 'J', sizeof(partitions[1].uniq_guid));
    strncpy(partitions[1].name, "This is partition1", sizeof(partitions[1].name));

    ASSERT_OK(device_add_metadata(parent_.get(), DEVICE_METADATA_PARTITION_MAP,
                                  partition_map_buffer.data(), partition_map_buffer.size()));

    ASSERT_OK(BootPartition::Bind(nullptr, parent_.get()));
    ASSERT_EQ(parent_->child_count(), 2);
  }

 protected:
  FakeBlockDevice fake_block_device_;
  std::shared_ptr<MockDevice> parent_ = MockDevice::FakeRootParent();
};

TEST_F(BootPartitionTest, BlockPartitionOps) {
  auto check_partition_info = [](BootPartition* driver, uint8_t guid_type_char,
                                 uint8_t guid_instance_char, const char* partition_name) {
    block_partition_protocol_t partition_proto;
    EXPECT_OK(driver->DdkGetProtocol(ZX_PROTOCOL_BLOCK_PARTITION, &partition_proto));
    ddk::BlockPartitionProtocolClient partition_client = {&partition_proto};
    ASSERT_TRUE(partition_client.is_valid());

    guid_t guid_type{};
    EXPECT_OK(partition_client.GetGuid(GUIDTYPE_TYPE, &guid_type));
    for (uint32_t i = 0; i < GUID_LENGTH; i++) {
      EXPECT_EQ(reinterpret_cast<char*>(&guid_type)[i], guid_type_char);
    }

    guid_t guid_instance{};
    EXPECT_OK(partition_client.GetGuid(GUIDTYPE_INSTANCE, &guid_instance));
    for (uint32_t i = 0; i < GUID_LENGTH; i++) {
      EXPECT_EQ(reinterpret_cast<char*>(&guid_instance)[i], guid_instance_char);
    }

    char name[MAX_PARTITION_NAME_LENGTH];
    EXPECT_OK(partition_client.GetName(name, sizeof(name)));
    EXPECT_STREQ(name, partition_name);
  };

  auto child0 = parent_->children().front();
  check_partition_info(child0->GetDeviceContext<BootPartition>(), 'T', 'I', "This is partition0");

  auto child1 = parent_->children().back();
  check_partition_info(child1->GetDeviceContext<BootPartition>(), 'U', 'J', "This is partition1");
}

TEST_F(BootPartitionTest, BlockImplOpsPassedThrough) {
  for (auto child : parent_->children()) {
    BootPartition* driver = child->GetDeviceContext<BootPartition>();

    block_impl_protocol_t block_proto;
    EXPECT_OK(driver->DdkGetProtocol(ZX_PROTOCOL_BLOCK_IMPL, &block_proto));
    ddk::BlockImplProtocolClient block_client = {&block_proto};
    ASSERT_TRUE(block_client.is_valid());

    block_info_t info{};
    uint64_t block_op_size = 0;
    block_client.Query(&info, &block_op_size);

    EXPECT_EQ(info.block_count, 12);
    EXPECT_EQ(info.block_size, 10);
    EXPECT_EQ(info.max_transfer_size, 10);
    EXPECT_EQ(block_op_size, sizeof(block_op_t));

    auto block_callback = [](void*, zx_status_t status, block_op_t*) { EXPECT_OK(status); };

    zx::vmo vmo;
    ASSERT_OK(zx::vmo::create(10, 0, &vmo));

    char buffer[10];
    strcpy(buffer, "Test data");
    EXPECT_OK(vmo.write(buffer, 0, sizeof(buffer)));

    block_op_t txn{
        .rw =
            {
                .command = {.opcode = BLOCK_OPCODE_WRITE, .flags = 0},
                .vmo = vmo.get(),
                .length = 1,
                .offset_dev = 0,
                .offset_vmo = 0,
            },
    };
    block_client.Queue(&txn, block_callback, nullptr);
    EXPECT_FALSE(fake_block_device_.flushed());  // FakeBlockDevice operates synchronously.

    txn = {
        .command = {.opcode = BLOCK_OPCODE_FLUSH, .flags = 0},
    };
    block_client.Queue(&txn, block_callback, nullptr);
    EXPECT_TRUE(fake_block_device_.flushed());  // FakeBlockDevice operates synchronously.

    EXPECT_STREQ(fake_block_device_.data(), "Test data");

    txn = {
        .rw =
            {
                .command = {.opcode = BLOCK_OPCODE_READ, .flags = 0},
                .vmo = vmo.get(),
                .length = 1,
                .offset_dev = 0,
                .offset_vmo = 0,
            },
    };
    block_client.Queue(&txn, block_callback, nullptr);

    EXPECT_OK(vmo.read(buffer, 0, sizeof(buffer)));
    EXPECT_STREQ(buffer, "Test data");
  }
}

}  // namespace bootpart
