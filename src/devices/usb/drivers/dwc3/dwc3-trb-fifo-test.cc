// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/usb/drivers/dwc3/dwc3-trb-fifo.h"

#include <lib/driver/fake-bti/cpp/fake-bti.h>

#include <gtest/gtest.h>

namespace dwc3 {

zx_status_t CacheFlush(dma_buffer::ContiguousBuffer* buffer, zx_off_t offset, size_t length) {
  return ZX_OK;
}

zx_status_t CacheFlushInvalidate(dma_buffer::ContiguousBuffer* buffer, zx_off_t offset,
                                 size_t length) {
  return ZX_OK;
}

class TrbFifoTest : public testing::Test {
 public:
  void SetUp() override {
    zx::result bti = fake_bti::CreateFakeBti();
    ASSERT_TRUE(bti.is_ok());
    bti_ = std::move(*bti);

    ASSERT_TRUE(fifo_.Init(bti_).is_ok());
  }

  void TearDown() override {
    fifo_.Release();
    bti_.reset();
  }

  void CheckLinkTRB() {
    zx_paddr_t first_phys = fifo_.GetPhys(fifo_.first_);
    EXPECT_EQ(fifo_.last_->ptr_low, (uint32_t)first_phys);
    EXPECT_EQ(fifo_.last_->ptr_high, (uint32_t)(first_phys >> 32));
    EXPECT_EQ(fifo_.last_->status, 0);
    EXPECT_EQ(fifo_.last_->control, TRB_TRBCTL_LINK | TRB_HWO);
  }

 protected:
  class TTrbFifo : public TrbFifo {
   public:
    using TrbFifo::first_;
    using TrbFifo::GetPhys;
    using TrbFifo::last_;
    using TrbFifo::read_;
    using TrbFifo::write_;
  };

  zx::bti bti_;
  TTrbFifo fifo_;
};

TEST_F(TrbFifoTest, Init) { CheckLinkTRB(); }

TEST_F(TrbFifoTest, WriteAndRead) {
  dwc3_trb_t* trb = fifo_.write_;
  trb->ptr_low = 0x1234;
  trb->ptr_high = 0x5678;
  trb->status = 0xabcd;
  trb->control = 0xef;
  fifo_.AdvanceWrite();

  dwc3_trb_t read_trb = fifo_.Read();
  EXPECT_EQ(read_trb.ptr_low, 0x1234);
  EXPECT_EQ(read_trb.ptr_high, 0x5678);
  EXPECT_EQ(read_trb.status, 0xabcd);
  EXPECT_EQ(read_trb.control, 0xef);

  fifo_.AdvanceRead();
  EXPECT_EQ(fifo_.read_, fifo_.write_);
}

TEST_F(TrbFifoTest, Wrap) {
  const size_t size = kBufferSize / sizeof(dwc3_trb_t);
  for (size_t i = 0; i < size - 1; i++) {
    fifo_.AdvanceWrite();
  }
  EXPECT_EQ(fifo_.write_, fifo_.first_);
}

TEST_F(TrbFifoTest, ReInitTest) {
  fifo_.AdvanceWrite();
  ASSERT_NE(fifo_.write_, fifo_.first_);

  fifo_.Release();
  ASSERT_TRUE(fifo_.Init(bti_).is_ok());

  ASSERT_EQ(fifo_.write_, fifo_.first_);
  ASSERT_EQ(fifo_.read_, fifo_.first_);

  CheckLinkTRB();
}

}  // namespace dwc3
