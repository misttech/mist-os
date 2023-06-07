// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "abr-shim.h"

#include <span>

#include <zxtest/zxtest.h>

#include "src/devices/testing/mock-ddk/mock-device.h"

namespace block {

class FakeBlockDevice : public ddk::BlockImplProtocol<FakeBlockDevice>,
                        public ddk::BlockPartitionProtocol<FakeBlockDevice> {
 public:
  FakeBlockDevice() { memset(data_, 0xff, sizeof(data_)); }

  char* data() { return data_; }
  bool flushed() const { return flushed_; }

  const block_impl_protocol_ops_t* BlockOps() const { return &block_impl_protocol_ops_; }
  const block_partition_protocol_ops_t* PartitionOps() const {
    return &block_partition_protocol_ops_;
  }

  void BlockImplQuery(block_info_t* out_info, uint64_t* out_block_op_size) {
    out_info->block_count = 1024;
    out_info->block_size = sizeof(data_);
    out_info->max_transfer_size = sizeof(data_) * 16;
    out_info->flags = 0;
    *out_block_op_size = sizeof(block_op_t);
  }

  void BlockImplQueue(block_op_t* txn, block_impl_queue_callback callback, void* cookie) {
    zx_status_t status = ZX_ERR_NOT_SUPPORTED;
    switch (txn->command.opcode) {
      case BLOCK_OPCODE_READ:
        if (txn->rw.length != 1 || txn->rw.offset_dev != 0) {
          status = ZX_ERR_OUT_OF_RANGE;
        } else {
          status = zx_vmo_write(txn->rw.vmo, data_, txn->rw.offset_vmo, sizeof(data_));
        }
        break;
      case BLOCK_OPCODE_WRITE:
        if (txn->rw.length != 1 || txn->rw.offset_dev != 0) {
          status = ZX_ERR_OUT_OF_RANGE;
        } else {
          status = zx_vmo_read(txn->rw.vmo, data_, txn->rw.offset_vmo, sizeof(data_));
          flushed_ = status != ZX_OK && flushed_;
        }
        break;
      case BLOCK_OPCODE_FLUSH:
        flushed_ = true;
        status = ZX_OK;
        break;
      default:
        break;
    }

    callback(cookie, status, txn);
  }

  zx_status_t BlockPartitionGetGuid(guidtype_t guid_type, guid_t* out_guid) {
    if (guid_type == GUIDTYPE_TYPE) {
      memset(out_guid, 'T', sizeof(*out_guid));
    } else if (guid_type == GUIDTYPE_INSTANCE) {
      memset(out_guid, 'I', sizeof(*out_guid));
    } else {
      return ZX_ERR_NOT_SUPPORTED;
    }
    return ZX_OK;
  }

  zx_status_t BlockPartitionGetName(char* out_name, size_t name_capacity) {
    constexpr char kPartitionName[] = "This is a partition";
    strncpy(out_name, kPartitionName, name_capacity);
    return ZX_OK;
  }

 private:
  char data_[512];
  bool flushed_ = true;
};

class AbrShimTest : public zxtest::Test {
 public:
  void SetUp() override {
    parent_->AddProtocol(ZX_PROTOCOL_BLOCK_IMPL, fake_block_device_.BlockOps(),
                         &fake_block_device_);
    parent_->AddProtocol(ZX_PROTOCOL_BLOCK_PARTITION, fake_block_device_.PartitionOps(),
                         &fake_block_device_);

    EXPECT_OK(AbrShim::Bind(nullptr, parent_.get()));
    ASSERT_EQ(parent_->child_count(), 1);

    dut_ = parent_->GetLatestChild()->GetDeviceContext<AbrShim>();

    block_impl_protocol_t block_proto;
    EXPECT_OK(dut_->DdkGetProtocol(ZX_PROTOCOL_BLOCK_IMPL, &block_proto));
    block_client_ = {&block_proto};
    ASSERT_TRUE(block_client_.is_valid());

    block_partition_protocol_t partition_proto;
    EXPECT_OK(dut_->DdkGetProtocol(ZX_PROTOCOL_BLOCK_PARTITION, &partition_proto));
    partition_client_ = {&partition_proto};
    ASSERT_TRUE(partition_client_.is_valid());
  }

 protected:
  FakeBlockDevice fake_block_device_;
  std::shared_ptr<MockDevice> parent_ = MockDevice::FakeRootParent();
  ddk::BlockImplProtocolClient block_client_;
  ddk::BlockPartitionProtocolClient partition_client_;
  AbrShim* dut_;
};

TEST_F(AbrShimTest, BlockOpsPassedThrough) {
  block_info_t info{};
  uint64_t block_op_size = 0;
  block_client_.Query(&info, &block_op_size);

  EXPECT_EQ(info.block_count, 1024);
  EXPECT_EQ(info.block_size, 512);
  EXPECT_EQ(info.max_transfer_size, 512 * 16);
  EXPECT_EQ(block_op_size, sizeof(block_op_t));

  auto block_callback = [](void*, zx_status_t status, block_op_t*) { EXPECT_OK(status); };

  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(512, 0, &vmo));

  char buffer[512];
  strcpy(buffer, "Write to block device");
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
  block_client_.Queue(&txn, block_callback, nullptr);

  txn = {
      .command = {.opcode = BLOCK_OPCODE_FLUSH, .flags = 0},
  };
  block_client_.Queue(&txn, block_callback, nullptr);

  EXPECT_STREQ(fake_block_device_.data(), "Write to block device");

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
  block_client_.Queue(&txn, block_callback, nullptr);

  EXPECT_OK(vmo.read(buffer, 0, sizeof(buffer)));
  EXPECT_STREQ(buffer, "Write to block device");

  guid_t guid{};
  EXPECT_OK(partition_client_.GetGuid(GUIDTYPE_TYPE, &guid));
  EXPECT_EQ(reinterpret_cast<char*>(&guid)[0], 'T');

  EXPECT_OK(partition_client_.GetName(buffer, sizeof(buffer)));
  EXPECT_STREQ(buffer, "This is a partition");
}

TEST_F(AbrShimTest, OneShotRecovery) {
  strcpy(fake_block_device_.data() + 256, "This should be preserved");

  parent_->GetLatestChild()->SuspendNewOp(0, false, DEVICE_SUSPEND_REASON_REBOOT_RECOVERY);
  EXPECT_TRUE(parent_->GetLatestChild()->SuspendReplyCalled());
  EXPECT_OK(parent_->GetLatestChild()->SuspendReplyCallStatus());

  EXPECT_TRUE(fake_block_device_.flushed());
  EXPECT_STREQ(fake_block_device_.data() + 256, "This should be preserved");

  // Check libabr magic.
  EXPECT_BYTES_EQ(fake_block_device_.data(), "\0AB0", 4);

  zx::vmo vmo;
  ASSERT_OK(zx::vmo::create(512, 0, &vmo));

  block_op_t txn{
      .rw =
          {
              .command = {.opcode = BLOCK_OPCODE_READ, .flags = 0},
              .vmo = vmo.get(),
              .length = 1,
              .offset_dev = 0,
              .offset_vmo = 0,
          },
  };

  // Subsequent block ops should be canceled.
  block_client_.Queue(
      &txn, [](void*, zx_status_t status, block_op_t*) { EXPECT_EQ(status, ZX_ERR_CANCELED); },
      nullptr);
}

TEST_F(AbrShimTest, NoOneShotRecoveryOnNormalReboot) {
  strcpy(fake_block_device_.data(), "This should be preserved");

  parent_->GetLatestChild()->SuspendNewOp(0, false, DEVICE_SUSPEND_REASON_REBOOT);
  EXPECT_TRUE(parent_->GetLatestChild()->SuspendReplyCalled());
  EXPECT_OK(parent_->GetLatestChild()->SuspendReplyCallStatus());

  EXPECT_STREQ(fake_block_device_.data(), "This should be preserved");
}

}  // namespace block
