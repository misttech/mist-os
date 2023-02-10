// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <endian.h>
#include <fuchsia/hardware/i2cimpl/cpp/banjo.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async/cpp/task.h>
#include <lib/ddk/metadata.h>
#include <lib/fzl/vmo-mapper.h>
#include <lib/mmio/mmio.h>
#include <lib/sync/completion.h>
#include <lib/zx/clock.h>

#include <array>
#include <atomic>
#include <vector>

#include <soc/aml-common/aml-i2c.h>
#include <zxtest/zxtest.h>

#include "src/devices/bus/testing/fake-pdev/fake-pdev.h"
#include "src/devices/testing/mock-ddk/mock-device.h"

extern "C" {
zx_status_t aml_i2c_bind(void* ctx, zx_device_t* parent);
}

namespace aml_i2c {

class AmlI2cTest : public zxtest::Test {
 public:
  void SetUp() override { EXPECT_OK(loop_.StartThread()); }

 protected:
  static constexpr size_t kControlReg = 0;
  static constexpr size_t kTargetAddrReg = 1;
  static constexpr size_t kTokenListReg = 2;
  static constexpr size_t kWriteDataReg = 4;
  static constexpr size_t kReadDataReg = 6;

  static constexpr size_t kMmioSize = sizeof(uint32_t) * 8;

  // TODO(fxbug.dev/120969): Include the headers that define these after the C++ conversion.
  enum Token : uint8_t {
    kEnd,
    kStart,
    kTargetAddrWr,
    kTargetAddrRd,
    kData,
    kDataLast,
    kStop,
  };

  struct AmlI2cDev {
    zx_handle_t irq;
    zx_handle_t event;
    mmio_buffer_t regs_iobuff;
    void* virt_regs;
    zx_duration_t timeout;
  };

  struct AmlI2c {
    pdev_protocol_t pdev;
    zx_device_t* zxdev;
    AmlI2cDev* i2c_devs;
    uint32_t dev_count;
  };

  uint32_t* mmio() const { return reinterpret_cast<uint32_t*>(mmio_mapper_.start()); }
  zx::unowned_interrupt irq(uint32_t index) const { return irqs_[index]->borrow(); }

  void InitResources(uint32_t bus_count) {
    root_->AddProtocol(ZX_PROTOCOL_PDEV, pdev_.proto()->ops, pdev_.proto()->ctx);
    EXPECT_OK(zx::vmo::create(kMmioSize, 0, &mmio_));

    for (uint32_t i = 0; i < bus_count; i++) {
      zx::vmo mmio;
      EXPECT_OK(mmio_.duplicate(ZX_RIGHT_SAME_RIGHTS, &mmio));
      // The offset field is not currently respected by aml-i2c.
      pdev_.set_mmio(i, {.vmo = std::move(mmio), .size = kMmioSize});
      irqs_.push_back(pdev_.CreateVirtualInterrupt(i));
    }

    pdev_.set_device_info({{.mmio_count = bus_count, .irq_count = bus_count}});
  }

  void Init(uint32_t bus_count) {
    EXPECT_NO_FATAL_FAILURE(InitResources(bus_count));

    EXPECT_OK(aml_i2c_bind(nullptr, root_.get()));
    ASSERT_EQ(root_->child_count(), 1);

    // Must map after aml_i2c_bind sets the VMO cache policy.
    EXPECT_OK(mmio_mapper_.Map(mmio_, 0, 0));

    AmlI2c* const i2c = root_->GetLatestChild()->GetDeviceContext<AmlI2c>();
    ASSERT_EQ(i2c->dev_count, bus_count);
    for (uint32_t i = 0; i < bus_count; i++) {
      i2c->i2c_devs[i].timeout = ZX_TIME_INFINITE;
    }
  }

  zx_status_t Transact(uint32_t bus_id, const i2c_impl_op_t* op_list, size_t op_count) {
    ddk::I2cImplProtocolClient i2c(root_->GetLatestChild());
    EXPECT_TRUE(i2c.is_valid());

    std::atomic_bool transfer_complete = false;
    sync_completion_t irq_loop_exit;

    // Continuously trigger interrupts for this controller until the transfer is complete.
    // Interrupts that come in after the transfer should be harmless.
    async::PostTask(loop_.dispatcher(), [=, &transfer_complete, irq_loop_exit = &irq_loop_exit]() {
      while (!transfer_complete) {
        irq(bus_id)->trigger(0, zx::clock::get_monotonic());
        zx::nanosleep(zx::deadline_after(zx::nsec(1)));
      }
      sync_completion_signal(irq_loop_exit);
    });

    zx_status_t status = i2c.Transact(bus_id, op_list, op_count);

    transfer_complete = true;
    sync_completion_wait(&irq_loop_exit, ZX_TIME_INFINITE);

    return status;
  }

  uint32_t ControllerAddress() const { return (mmio()[kTargetAddrReg] >> 1) & 0x7f; }

  void SetReadData(std::vector<uint8_t> read_bytes) {
    uint64_t read_data = 0;
    EXPECT_LE(read_bytes.size(), sizeof(read_data));
    memcpy(&read_data, read_bytes.data(), sizeof(read_data));
    *reinterpret_cast<uint64_t*>(mmio() + kReadDataReg) = le64toh(read_data);
  }

  void ExpectWriteDataEq(const std::vector<uint8_t>& expected) const {
    std::array<uint8_t, 8> actual;
    const uint64_t write_data = le64toh(*reinterpret_cast<uint64_t*>(mmio() + kWriteDataReg));
    memcpy(actual.data(), &write_data, sizeof(write_data));

    ASSERT_LE(expected.size(), actual.size());
    EXPECT_BYTES_EQ(actual.data(), expected.data(), expected.size());
  }

  void ExpectTokenListEq(const std::vector<uint8_t>& expected) const {
    std::array<uint8_t, 16> actual;
    uint64_t token_data = le64toh(*reinterpret_cast<uint64_t*>(mmio() + kTokenListReg));
    for (size_t i = 0; i < actual.size(); i++, token_data >>= 4) {
      actual[i] = token_data & 0xf;
    }

    ASSERT_LE(expected.size(), actual.size());
    EXPECT_BYTES_EQ(actual.data(), expected.data(), expected.size());
  }

  std::vector<zx::unowned_interrupt> irqs_;
  fake_pdev::FakePDev pdev_;
  std::shared_ptr<MockDevice> root_ = MockDevice::FakeRootParent();
  zx::vmo mmio_;

 private:
  fzl::VmoMapper mmio_mapper_;
  async::Loop loop_{&kAsyncLoopConfigNeverAttachToThread};
};

// TODO(fxbug.dev/120969): Re-enable after the driver bugs have been fixed.
TEST_F(AmlI2cTest, DISABLED_SmallWrite) {
  EXPECT_NO_FATAL_FAILURE(Init(1));

  uint8_t write_buffer[] = {0x45, 0xd9, 0x65, 0xbc, 0x31, 0x26, 0xd7, 0xe5};
  const i2c_impl_op_t op{
      .address = 0x13,
      .data_buffer = write_buffer,
      .data_size = sizeof(write_buffer),
      .is_read = false,
      .stop = true,
  };

  EXPECT_OK(Transact(0, &op, 1));

  EXPECT_EQ(ControllerAddress(), 0x13);
  ExpectWriteDataEq({0x45, 0xd9, 0x65, 0xbc, 0x31, 0x26, 0xd7, 0xe5});
  ExpectTokenListEq({
      Token::kStart,
      Token::kTargetAddrWr,
      Token::kData,
      Token::kData,
      Token::kData,
      Token::kData,
      Token::kData,
      Token::kData,
      Token::kData,
      Token::kData,
      Token::kStop,
      Token::kEnd,
  });
}

TEST_F(AmlI2cTest, DISABLED_BigWrite) {
  EXPECT_NO_FATAL_FAILURE(Init(1));

  uint8_t write_buffer[] = {0xb9, 0x17, 0x32, 0xba, 0x8e, 0xf7, 0x19, 0xf2, 0x78, 0xbf,
                            0xcb, 0xd3, 0xdc, 0xad, 0xbd, 0x78, 0x1b, 0xa8, 0xef, 0x1a};
  const i2c_impl_op_t op{
      .address = 0x5f,
      .data_buffer = write_buffer,
      .data_size = sizeof(write_buffer),
      .is_read = false,
      .stop = true,
  };

  EXPECT_OK(Transact(0, &op, 1));

  EXPECT_EQ(ControllerAddress(), 0x5f);
  ExpectWriteDataEq({0x1b, 0xa8, 0xef, 0x1a, 0x00, 0x00, 0x00, 0x00});
  ExpectTokenListEq({
      Token::kData,
      Token::kData,
      Token::kData,
      Token::kData,
      Token::kStop,
      Token::kEnd,
  });
}

TEST_F(AmlI2cTest, DISABLED_SmallRead) {
  EXPECT_NO_FATAL_FAILURE(Init(1));

  const std::vector<uint8_t> kExpectedReadData{0xf0, 0xdb, 0xdf, 0x6b, 0xb9, 0x3e, 0xa6, 0xfa};
  SetReadData(kExpectedReadData);

  std::array<uint8_t, 8> read_buffer;
  memset(read_buffer.data(), 0xaa, read_buffer.size());
  const i2c_impl_op_t op{
      .address = 0x41,
      .data_buffer = read_buffer.data(),
      .data_size = read_buffer.size(),
      .is_read = true,
      .stop = true,
  };

  EXPECT_OK(Transact(0, &op, 1));

  // TODO(fxbug.dev/120969): Registers are cleared before each read, so we can't validate the
  // received data. After the C++ conversion we should be able to do this with fake-mmio-reg.
  EXPECT_EQ(ControllerAddress(), 0x41);
  EXPECT_TRUE(std::all_of(read_buffer.cbegin(), read_buffer.cend(), [](auto i) { return !i; }));
  ExpectTokenListEq({
      Token::kStart,
      Token::kTargetAddrRd,
      Token::kData,
      Token::kData,
      Token::kData,
      Token::kData,
      Token::kData,
      Token::kData,
      Token::kData,
      Token::kDataLast,
      Token::kStop,
      Token::kEnd,
  });
}

TEST_F(AmlI2cTest, DISABLED_BigRead) {
  EXPECT_NO_FATAL_FAILURE(Init(1));

  const std::vector<uint8_t> kExpectedReadData{0xd8, 0xfa, 0x51, 0xc7, 0x8c, 0xd9, 0x32, 0x92};
  SetReadData(kExpectedReadData);

  std::array<uint8_t, 20> read_buffer;
  memset(read_buffer.data(), 0xaa, read_buffer.size());
  const i2c_impl_op_t op{
      .address = 0x29,
      .data_buffer = read_buffer.data(),
      .data_size = read_buffer.size(),
      .is_read = true,
      .stop = true,
  };

  EXPECT_OK(Transact(0, &op, 1));

  EXPECT_EQ(ControllerAddress(), 0x29);
  EXPECT_TRUE(std::all_of(read_buffer.cbegin(), read_buffer.cend(), [](auto i) { return !i; }));
  ExpectTokenListEq({
      Token::kData,
      Token::kData,
      Token::kData,
      Token::kDataLast,
      Token::kStop,
      Token::kEnd,
  });
}

TEST_F(AmlI2cTest, DISABLED_NoStopFlag) {
  EXPECT_NO_FATAL_FAILURE(Init(1));

  uint8_t buffer[4];
  const i2c_impl_op_t op{
      .data_buffer = buffer,
      .data_size = sizeof(buffer),
      .is_read = false,
      .stop = false,
  };

  EXPECT_OK(Transact(0, &op, 1));

  ExpectTokenListEq({
      Token::kStart,
      Token::kTargetAddrWr,
      Token::kData,
      Token::kData,
      Token::kData,
      Token::kData,
      Token::kEnd,
  });
}

TEST_F(AmlI2cTest, DISABLED_StartTransferFlag) {
  EXPECT_NO_FATAL_FAILURE(Init(1));

  uint8_t buffer[4];
  const i2c_impl_op_t op{
      .data_buffer = buffer,
      .data_size = sizeof(buffer),
      .is_read = true,
      .stop = false,
  };

  EXPECT_EQ(mmio()[kControlReg], 0);

  EXPECT_OK(Transact(0, &op, 1));

  EXPECT_EQ(mmio()[kControlReg], 1);
}

TEST_F(AmlI2cTest, DISABLED_TransferError) {
  EXPECT_NO_FATAL_FAILURE(Init(1));

  uint8_t buffer[4];
  const i2c_impl_op_t op{
      .data_buffer = buffer,
      .data_size = sizeof(buffer),
      .is_read = true,
      .stop = false,
  };

  mmio()[kControlReg] = 1 << 3;

  EXPECT_NOT_OK(Transact(0, &op, 1));
}

// TODO(fxbug.dev/120969): Re-enable after the bug has been fixed.
TEST_F(AmlI2cTest, DISABLED_ManyTransactions) {
  EXPECT_NO_FATAL_FAILURE(Init(1));

  const std::vector<uint8_t> kExpectedReadData{0x41, 0x6a, 0x51, 0x4f, 0xca, 0xd3, 0x5b, 0xe5};
  SetReadData(kExpectedReadData);

  std::array<uint8_t, 20> read_buffer_1;
  std::array<uint8_t, 4> read_buffer_2;
  memset(read_buffer_1.data(), 0xaa, read_buffer_1.size());
  memset(read_buffer_2.data(), 0xaa, read_buffer_2.size());

  std::array<uint8_t, 8> write_buffer_1{0x39};
  std::array<uint8_t, 8> write_buffer_2{0xf0, 0xf9, 0x17, 0xad, 0x51, 0xdc, 0x30, 0xe5};

  const i2c_impl_op_t ops[]{
      {
          .address = 0x1c,
          .data_buffer = write_buffer_1.data(),
          .data_size = write_buffer_1.size(),
          .is_read = false,
          .stop = false,
      },
      {
          .address = 0x2d,
          .data_buffer = read_buffer_1.data(),
          .data_size = read_buffer_1.size(),
          .is_read = true,
          .stop = true,
      },
      {
          .address = 0x3e,
          .data_buffer = write_buffer_2.data(),
          .data_size = write_buffer_2.size(),
          .is_read = false,
          .stop = true,
      },
      {
          .address = 0x4f,
          .data_buffer = read_buffer_2.data(),
          .data_size = read_buffer_2.size(),
          .is_read = true,
          .stop = false,
      },
  };

  EXPECT_OK(Transact(0, ops, std::size(ops)));

  EXPECT_EQ(ControllerAddress(), 0x4f);
  EXPECT_TRUE(std::all_of(read_buffer_1.cbegin(), read_buffer_1.cend(), [](auto i) { return !i; }));
  EXPECT_TRUE(std::all_of(read_buffer_2.cbegin(), read_buffer_2.cend(), [](auto i) { return !i; }));
  ExpectWriteDataEq({0xf0, 0xf9, 0x17, 0xad, 0x51, 0xdc, 0x30, 0xe5});
  ExpectTokenListEq({
      Token::kStart,
      Token::kTargetAddrRd,
      Token::kData,
      Token::kData,
      Token::kData,
      Token::kDataLast,
      Token::kEnd,
  });
}

TEST_F(AmlI2cTest, DISABLED_MultipleControllers) {
  EXPECT_NO_FATAL_FAILURE(Init(4));

  uint8_t write_buffer[] = {0x45};
  const i2c_impl_op_t op{
      .address = 0x56,
      .data_buffer = write_buffer,
      .data_size = sizeof(write_buffer),
      .is_read = false,
  };

  EXPECT_OK(Transact(0, &op, 1));
  EXPECT_EQ(ControllerAddress(), 0x56);
  ExpectWriteDataEq({0x45});
  ExpectTokenListEq({Token::kStart, Token::kTargetAddrWr, Token::kData, Token::kEnd});

  EXPECT_OK(Transact(1, &op, 1));
  EXPECT_EQ(ControllerAddress(), 0x56);
  ExpectWriteDataEq({0x45});
  ExpectTokenListEq({Token::kStart, Token::kTargetAddrWr, Token::kData, Token::kEnd});

  EXPECT_OK(Transact(2, &op, 1));
  EXPECT_EQ(ControllerAddress(), 0x56);
  ExpectWriteDataEq({0x45});
  ExpectTokenListEq({Token::kStart, Token::kTargetAddrWr, Token::kData, Token::kEnd});

  EXPECT_OK(Transact(3, &op, 1));
  EXPECT_EQ(ControllerAddress(), 0x56);
  ExpectWriteDataEq({0x45});
  ExpectTokenListEq({Token::kStart, Token::kTargetAddrWr, Token::kData, Token::kEnd});

  ddk::I2cImplProtocolClient i2c(root_->GetLatestChild());
  EXPECT_TRUE(i2c.is_valid());
  EXPECT_NOT_OK(i2c.Transact(4, &op, 1));
}

TEST_F(AmlI2cTest, DISABLED_TransactionTooBig) {
  EXPECT_NO_FATAL_FAILURE(Init(1));

  uint8_t buffer[513];
  i2c_impl_op_t op{
      .data_buffer = buffer,
      .data_size = 512,
      .is_read = false,
  };
  EXPECT_OK(Transact(0, &op, 1));

  op.data_size = 513;
  EXPECT_NOT_OK(Transact(0, &op, 1));
}

TEST_F(AmlI2cTest, DISABLED_Metadata) {
  aml_i2c_delay_values metadata[]{
      {
          .quarter_clock_delay = 0,
          .clock_low_delay = 0,
      },
      {
          .quarter_clock_delay = 0x3cd,
          .clock_low_delay = 0xf12,
      },
      {
          .quarter_clock_delay = 0,
          .clock_low_delay = 0,
      },
  };

  root_->SetMetadata(DEVICE_METADATA_PRIVATE, &metadata, sizeof(metadata));

  EXPECT_NO_FATAL_FAILURE(Init(3));

  EXPECT_EQ(mmio()[kControlReg], 0x3cd << 12);
  EXPECT_EQ(mmio()[kTargetAddrReg], (0xf12 << 16) | (1 << 28));
}

TEST_F(AmlI2cTest, DISABLED_NoMetadata) {
  EXPECT_NO_FATAL_FAILURE(Init(3));

  EXPECT_EQ(mmio()[kControlReg], 0);
  EXPECT_EQ(mmio()[kTargetAddrReg], 0);
}

TEST_F(AmlI2cTest, MetadataTooBig) {
  aml_i2c_delay_values metadata[]{
      {
          .quarter_clock_delay = 0,
          .clock_low_delay = 0,
      },
      {
          .quarter_clock_delay = 0x3cd,
          .clock_low_delay = 0xf12,
      },
      {
          .quarter_clock_delay = 0,
          .clock_low_delay = 0,
      },
      {
          .quarter_clock_delay = 0,
          .clock_low_delay = 0,
      },
  };

  root_->SetMetadata(DEVICE_METADATA_PRIVATE, &metadata, sizeof(metadata));

  EXPECT_NO_FATAL_FAILURE(InitResources(3));

  EXPECT_NOT_OK(aml_i2c_bind(nullptr, root_.get()));
}

TEST_F(AmlI2cTest, MetadataTooSmall) {
  aml_i2c_delay_values metadata[]{
      {
          .quarter_clock_delay = 0,
          .clock_low_delay = 0,
      },
      {
          .quarter_clock_delay = 0x3cd,
          .clock_low_delay = 0xf12,
      },
  };

  root_->SetMetadata(DEVICE_METADATA_PRIVATE, &metadata, sizeof(metadata));

  EXPECT_NO_FATAL_FAILURE(InitResources(3));

  EXPECT_NOT_OK(aml_i2c_bind(nullptr, root_.get()));
}

TEST_F(AmlI2cTest, DISABLED_CanUsePDevFragment) {
  root_->AddProtocol(ZX_PROTOCOL_PDEV, pdev_.proto()->ops, pdev_.proto()->ctx, "pdev");
  EXPECT_OK(zx::vmo::create(kMmioSize, 0, &mmio_));

  zx::vmo mmio;
  EXPECT_OK(mmio_.duplicate(ZX_RIGHT_SAME_RIGHTS, &mmio));
  pdev_.set_mmio(0, {.vmo = std::move(mmio), .size = kMmioSize});
  irqs_.push_back(pdev_.CreateVirtualInterrupt(0));

  pdev_.set_device_info({{.mmio_count = 1, .irq_count = 1}});

  EXPECT_OK(aml_i2c_bind(nullptr, root_.get()));
  ASSERT_EQ(root_->child_count(), 1);
}

TEST_F(AmlI2cTest, MmioIrqMismatch) {
  root_->AddProtocol(ZX_PROTOCOL_PDEV, pdev_.proto()->ops, pdev_.proto()->ctx, "pdev");

  pdev_.set_device_info({{.mmio_count = 2, .irq_count = 1}});

  EXPECT_NOT_OK(aml_i2c_bind(nullptr, root_.get()));
}

}  // namespace aml_i2c
