// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "../block.h"

#include <fbl/string.h>
#include <zxtest/zxtest.h>

#include "../usb-mass-storage.h"
#include "src/devices/testing/mock-ddk/mock-device.h"

namespace {

class UmsBlockTest : public zxtest::Test {
 public:
  UmsBlockTest() : parent_(MockDevice::FakeRootParent()) {}

  void SetUp() override {
    dev_ = std::make_unique<ums::UmsBlockDevice>(
        parent_.get(), 5, [&](ums::Transaction* txn) { context_.txn = txn; });
    ums::BlockDeviceParameters params = {};
    params.lun = 5;
    EXPECT_TRUE(params == dev_->GetBlockDeviceParameters(),
                "Parameters must be set to user-provided values.");
    dev_->Adopt();
  }

  void TearDown() override {
    device_async_remove(dev_->zxdev());
    mock_ddk::ReleaseFlaggedDevices(parent_.get());
  }

 protected:
  struct Context {
    block_op_t* op;
    zx_status_t status;
    ums::Transaction* txn;
  };

  std::shared_ptr<zx_device> parent_;
  std::unique_ptr<ums::UmsBlockDevice> dev_;
  Context context_;

  static void BlockCallback(void* ctx, zx_status_t status, block_op_t* op) {
    Context* context = reinterpret_cast<Context*>(ctx);
    context->status = status;
    context->op = op;
  }
};

TEST_F(UmsBlockTest, AddTest) { EXPECT_EQ(ZX_OK, dev_->Add(), "Expected Add to succeed"); }

TEST_F(UmsBlockTest, NotSupportedTest) {
  EXPECT_EQ(ZX_OK, dev_->Add(), "Expected Add to succeed");
  MockDevice* child = parent_->GetLatestChild();
  EXPECT_TRUE(fbl::String("lun-005") == child->name());
  ums::Transaction txn;
  txn.op.command = BLOCK_OP_MASK;
  dev_->BlockImplQueue(&txn.op, BlockCallback, &context_);
  EXPECT_EQ(ZX_ERR_NOT_SUPPORTED, context_.status);
}

TEST_F(UmsBlockTest, ReadTest) {
  EXPECT_EQ(ZX_OK, dev_->Add(), "Expected Add to succeed");
  MockDevice* child = parent_->GetLatestChild();
  EXPECT_TRUE(fbl::String("lun-005") == child->name());
  ums::Transaction txn;
  txn.op.command = BLOCK_OP_READ;
  dev_->BlockImplQueue(&txn.op, BlockCallback, &context_);
}

TEST_F(UmsBlockTest, WriteTest) {
  EXPECT_EQ(ZX_OK, dev_->Add(), "Expected Add to succeed");
  MockDevice* child = parent_->GetLatestChild();
  EXPECT_TRUE(fbl::String("lun-005") == child->name());
  ums::Transaction txn;
  txn.op.command = BLOCK_OP_WRITE;
  dev_->BlockImplQueue(&txn.op, BlockCallback, &context_);
  EXPECT_EQ(&txn, context_.txn);
}

TEST_F(UmsBlockTest, FlushTest) {
  EXPECT_EQ(ZX_OK, dev_->Add(), "Expected Add to succeed");
  MockDevice* child = parent_->GetLatestChild();
  EXPECT_TRUE(fbl::String("lun-005") == child->name());
  ums::Transaction txn;
  txn.op.command = BLOCK_OP_FLUSH;
  dev_->BlockImplQueue(&txn.op, BlockCallback, &context_);
  EXPECT_EQ(&txn, context_.txn);
}

}  // namespace
