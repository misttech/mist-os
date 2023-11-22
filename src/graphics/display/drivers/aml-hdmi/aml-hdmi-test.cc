// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/aml-hdmi/aml-hdmi.h"

#include <fidl/fuchsia.hardware.hdmi/cpp/wire.h>
#include <fuchsia/hardware/i2cimpl/c/banjo.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/fidl/cpp/wire/vector_view.h>

#include <list>
#include <variant>

#include <fbl/alloc_checker.h>
#include <fbl/array.h>
#include <gmock/gmock.h>
#include <gtest/gtest.h>
#include <mock-mmio-range/mock-mmio-range.h>

#include "src/graphics/display/lib/api-types-cpp/display-timing.h"
#include "src/lib/testing/predicates/status.h"

namespace aml_hdmi {

namespace {

struct I2cReadTransaction {
  uint16_t address;
  size_t read_size_bytes;
};

struct I2cWriteTransaction {
  uint16_t address;
  std::vector<uint8_t> data;
};

using I2cTransaction = std::variant<I2cReadTransaction, I2cWriteTransaction>;

// TODO(fxbug.dev/136257): Consider replacing the mock class with a fake
// HdmiTransmitterController implementation instead.
class MockHdmiTransmitterController : public designware_hdmi::HdmiTransmitterController {
 public:
  MockHdmiTransmitterController() = default;
  ~MockHdmiTransmitterController() override { EXPECT_EQ(expected_dw_i2c_transactions_.size(), 0u); }

  zx_status_t InitHw() override { return ZX_OK; }
  zx_status_t EdidTransfer(const i2c_impl_op_t* op_list, size_t op_count) override {
    I2cTransact(op_list, op_count);
    return ZX_OK;
  }

  void ConfigHdmitx(const designware_hdmi::ColorParam& color_param,
                    const display::DisplayTiming& mode,
                    const designware_hdmi::hdmi_param_tx& p) override {}
  void SetupInterrupts() override {}
  void Reset() override {}
  void SetupScdc(bool is4k) override {}
  void ResetFc() override {}
  void SetFcScramblerCtrl(bool is4k) override {}
  void PrintRegisters() override {}

  void ExpectHdmiDwI2cRead(uint16_t address, size_t read_size_bytes) {
    expected_dw_i2c_transactions_.push_back(I2cReadTransaction{
        .address = address,
        .read_size_bytes = read_size_bytes,
    });
  }

  void ExpectHdmiDwI2cWrite(uint16_t address, std::vector<uint8_t> data) {
    expected_dw_i2c_transactions_.push_back(I2cWriteTransaction{
        .address = address,
        .data = std::move(data),
    });
  }

 private:
  void I2cTransact(const i2c_impl_op_t* op_list, size_t op_count) {
    cpp20::span<const i2c_impl_op_t> ops(op_list, op_count);
    for (const i2c_impl_op_t& op : ops) {
      ASSERT_FALSE(expected_dw_i2c_transactions_.empty());
      I2cTransaction& expected_transaction = expected_dw_i2c_transactions_.front();

      if (op.is_read) {
        ASSERT_TRUE(std::holds_alternative<I2cReadTransaction>(expected_transaction));
        const I2cReadTransaction& expected_read =
            std::get<I2cReadTransaction>(expected_transaction);
        EXPECT_EQ(expected_read.address, op.address);
        EXPECT_EQ(expected_read.read_size_bytes, op.data_size);
      } else {
        ASSERT_TRUE(std::holds_alternative<I2cWriteTransaction>(expected_transaction));
        const I2cWriteTransaction& expected_write =
            std::get<I2cWriteTransaction>(expected_transaction);
        cpp20::span<const uint8_t> actual_data(op.data_buffer, op.data_size);
        EXPECT_EQ(expected_write.address, op.address);
        EXPECT_THAT(expected_write.data, testing::ElementsAreArray(actual_data));
      }
      expected_dw_i2c_transactions_.pop_front();
    }
  }
  std::list<I2cTransaction> expected_dw_i2c_transactions_;
};

class AmlHdmiTest : public testing::Test {
 public:
  void SetUp() override {
    loop_.StartThread("aml-hdmi-test-thread");

    auto mock_hdmitx_controller = std::make_unique<MockHdmiTransmitterController>();
    mock_hdmitx_controller_ = mock_hdmitx_controller.get();

    // TODO(fxbug.dev/123426): Use a fake SMC resource, when the
    // implementation lands.
    dut_ =
        std::make_unique<AmlHdmiDevice>(nullptr, top_level_mmio_range_.GetMmioBuffer(),
                                        std::move(mock_hdmitx_controller), /*smc=*/zx::resource{});
    ASSERT_TRUE(dut_);

    auto endpoints = fidl::CreateEndpoints<fuchsia_hardware_hdmi::Hdmi>();
    ASSERT_TRUE(endpoints.is_ok());

    binding_ = fidl::BindServer(loop_.dispatcher(), std::move(endpoints->server), dut_.get());
    hdmi_client_.Bind(std::move(endpoints->client));
  }

  void TearDown() override {}

 protected:
  constexpr static int kTopLevelMmioRangeSize = 0x8000;
  ddk_mock::MockMmioRange top_level_mmio_range_{kTopLevelMmioRangeSize,
                                                ddk_mock::MockMmioRange::Size::k32};

  std::unique_ptr<AmlHdmiDevice> dut_;

  // Owned by `dut_`.
  MockHdmiTransmitterController* mock_hdmitx_controller_ = nullptr;

  async::Loop loop_{&kAsyncLoopConfigNeverAttachToThread};
  std::optional<fidl::ServerBindingRef<fuchsia_hardware_hdmi::Hdmi>> binding_;

  fidl::WireSyncClient<fuchsia_hardware_hdmi::Hdmi> hdmi_client_;
};

}  // namespace

// Verify that the EDID I2c transactions received by the HdmiTransmitter
// and the HdmiTransmitterController IP core are the same as transactions
// issued by the client.
TEST_F(AmlHdmiTest, EdidTransfer) {
  mock_hdmitx_controller_->ExpectHdmiDwI2cWrite(0x60, {0});
  mock_hdmitx_controller_->ExpectHdmiDwI2cRead(0xa1, 128);
  mock_hdmitx_controller_->ExpectHdmiDwI2cRead(0xa5, 128);
  mock_hdmitx_controller_->ExpectHdmiDwI2cWrite(0x60, {1});
  mock_hdmitx_controller_->ExpectHdmiDwI2cRead(0xa1, 128);
  mock_hdmitx_controller_->ExpectHdmiDwI2cRead(0xa5, 128);

  static constexpr fuchsia_hardware_hdmi::wire::EdidOp kOps[] = {
      {
          .address = 0x60,
          .is_write = true,
      },
      {
          .address = 0xa1,
          .is_write = false,
      },
      {
          .address = 0xa5,
          .is_write = false,
      },
      {
          .address = 0x60,
          .is_write = true,
      },
      {
          .address = 0xa1,
          .is_write = false,
      },
      {
          .address = 0xa5,
          .is_write = false,
      },
  };
  static constexpr uint8_t kZero[] = {0};
  static constexpr uint8_t kOne[] = {1};
  static constexpr uint16_t kReadSegmentsLength[] = {128, 128, 128, 128};

  fidl::Arena arena;
  fidl::WireResult result = hdmi_client_->EdidTransfer(
      fidl::VectorView<fuchsia_hardware_hdmi::wire::EdidOp>(arena, kOps),
      fidl::VectorView<fidl::VectorView<uint8_t>>(arena, {{arena, kZero}, {arena, kOne}}),
      fidl::VectorView<uint16_t>(arena, kReadSegmentsLength));

  ASSERT_OK(result.status());
}

}  // namespace aml_hdmi
