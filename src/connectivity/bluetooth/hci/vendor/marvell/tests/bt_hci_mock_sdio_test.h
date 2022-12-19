// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_BLUETOOTH_HCI_VENDOR_MARVELL_TESTS_BT_HCI_MOCK_SDIO_TEST_H_
#define SRC_CONNECTIVITY_BLUETOOTH_HCI_VENDOR_MARVELL_TESTS_BT_HCI_MOCK_SDIO_TEST_H_

#include <fuchsia/hardware/sdio/cpp/banjo-mock.h>

#include <zxtest/zxtest.h>

#include "src/connectivity/bluetooth/hci/vendor/marvell/bt_hci_marvell.h"
#include "src/connectivity/bluetooth/hci/vendor/marvell/tests/bt_hci_mock_sdio.h"
#include "src/devices/testing/mock-ddk/mock-device.h"

namespace bt_hci_marvell {

class BtHciMockSdioTest : public zxtest::Test {
 public:
  // 88W8987
  static constexpr uint32_t kSimProductId = 0x914a;

  // The value we'll return for reads from the ioport address registers
  static constexpr uint32_t kSimIoportAddr = 0x4a4d43;
  static constexpr uint8_t kSimIoportAddrLow = kSimIoportAddr & 0xff;
  static constexpr uint8_t kSimIoportAddrMid = (kSimIoportAddr & 0xff00) >> CHAR_BIT;
  static constexpr uint8_t kSimIoportAddrHigh = (kSimIoportAddr & 0xff0000) >> (CHAR_BIT * 2);

  // Go through all of the steps to set up our test environment: create our two devices (SDIO fake
  // and our dut) and driver interfaces (SDIO fake and our own driver). Performs the simulated bind
  // and calls DdkInit().
  void SetUp() override {
    zx::result<std::unique_ptr<DeviceOracle>> result;
    result = DeviceOracle::Create(kSimProductId);
    ASSERT_TRUE(result.is_ok());
    device_oracle_ = std::move(result.value());

    zxtest::Test::SetUp();

    // Construct the root of our mock device tree
    const sdio_protocol_t* sdio_proto = mock_sdio_device_.GetProto();
    fake_parent_->AddProtocol(ZX_PROTOCOL_SDIO, sdio_proto->ops, sdio_proto->ctx);

    // Verify basic device initialization
    sdio_hw_info_t hw_info;
    hw_info.func_hw_info.product_id = kSimProductId;
    uint16_t expected_block_size = device_oracle_->GetSdioBlockSize();
    mock_sdio_device_.ExpectGetDevHwInfo(ZX_OK, hw_info)
        .ExpectEnableFn(ZX_OK)
        .ExpectUpdateBlockSize(ZX_OK, expected_block_size, /* deflt */ false);

    // Expect a read from the firmware status registers
    uint32_t fw_status_addr = device_oracle_->GetRegAddrFirmwareStatus();
    mock_sdio_device_
        .ExpectDoRwByte(ZX_OK, /* write */ false, fw_status_addr, /* write_byte */ 0,
                        kFirmwareStatusReady & 0xff)
        .ExpectDoRwByte(ZX_OK, /* write */ false, fw_status_addr + 1,
                        /* write_byte */ 0, (kFirmwareStatusReady & 0xff00) >> CHAR_BIT);

    // Read the IOPort address
    uint32_t ioport_addr = device_oracle_->GetRegAddrIoportAddr();
    mock_sdio_device_
        .ExpectDoRwByte(ZX_OK, /* write */ false, ioport_addr, /* write_byte */ 0,
                        kSimIoportAddrLow)
        .ExpectDoRwByte(ZX_OK, /* write */ false, ioport_addr + 1, /* write_byte */ 0,
                        kSimIoportAddrMid)
        .ExpectDoRwByte(ZX_OK, /* write */ false, ioport_addr + 2, /* write_byte */ 0,
                        kSimIoportAddrHigh);

    // Set "clear-on-read" bits
    uint32_t rsr_addr = device_oracle_->GetRegAddrInterruptRsr();
    uint8_t rsr_value = 0;
    mock_sdio_device_.ExpectDoRwByte(ZX_OK, /* write */ false, rsr_addr, /* write_byte */ 0,
                                     /* out_read_byte */ rsr_value);
    rsr_value &= ~kRsrClearOnReadMask;
    rsr_value |= kRsrClearOnReadValue;
    mock_sdio_device_.ExpectDoRwByte(ZX_OK, /* write */ true, rsr_addr, /* write_byte */ rsr_value,
                                     /* out_read_byte */ 0);

    // Set the "automatically re-enable interrupts" bits
    uint32_t misc_cfg_addr = device_oracle_->GetRegAddrMiscCfg();
    uint8_t misc_cfg_value = 0;
    mock_sdio_device_.ExpectDoRwByte(ZX_OK, /* write */ false, misc_cfg_addr, /* write_byte */ 0,
                                     /* out_read_byte */ misc_cfg_value);
    misc_cfg_value &= ~kMiscCfgAutoReenableMask;
    misc_cfg_value |= kMiscCfgAutoReenableValue;
    mock_sdio_device_.ExpectDoRwByte(ZX_OK, /* write */ true, misc_cfg_addr,
                                     /* write_byte */ misc_cfg_value, /* out_read_byte */ 0);

    // Create our driver instance, which should bind to the SDIO mock
    ASSERT_EQ(ZX_OK, BtHciMarvell::Bind(/* ctx */ nullptr, fake_parent_.get()));

    // Find the device that represents our Marvell device in the mock device tree
    dut_ = fake_parent_->GetLatestChild();
    ASSERT_NOT_NULL(dut_);

    // Find the Marvell driver
    driver_instance_ = dut_->GetDeviceContext<BtHciMarvell>();
    ASSERT_NOT_NULL(driver_instance_);

    // Call DdkInit
    Init();

    // Run all of the checks to verify that the expected calls into the mock SDIO API were made
    // during initialization
    ASSERT_NO_FATAL_FAILURE(mock_sdio_device_.VerifyAndClear());
  }

  void TearDown() override {
    // Deconstruct the mock device tree
    UnbindAndRelease();

    zxtest::Test::TearDown();
  }

  void Init() {
    // Invoke ddkInit()
    dut_->InitOp();

    // Verify that the Init operation returned a completion status
    ASSERT_EQ(ZX_OK, dut_->WaitUntilInitReplyCalled());

    // Verify that the status was success
    ASSERT_EQ(ZX_OK, dut_->InitReplyCallStatus());
  }

  void UnbindAndRelease() {
    // Flag our mock device for removal
    device_async_remove(dut_);

    // Remove from the mock device tree
    mock_ddk::ReleaseFlaggedDevices(fake_parent_.get());
  }

 protected:
  BtHciMarvell* driver_instance_;

  // The root of our MockDevice tree
  std::shared_ptr<MockDevice> fake_parent_ = MockDevice::FakeRootParent();

  // This is the object that will handle all of our SDIO calls to the fake parent
  BtHciMockSdio mock_sdio_device_;

 private:
  std::unique_ptr<DeviceOracle> device_oracle_;
  MockDevice* dut_;
};

}  // namespace bt_hci_marvell

#endif  // SRC_CONNECTIVITY_BLUETOOTH_HCI_VENDOR_MARVELL_TESTS_BT_HCI_MOCK_SDIO_TEST_H_
