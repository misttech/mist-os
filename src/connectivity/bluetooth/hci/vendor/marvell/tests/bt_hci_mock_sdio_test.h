// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_BLUETOOTH_HCI_VENDOR_MARVELL_TESTS_BT_HCI_MOCK_SDIO_TEST_H_
#define SRC_CONNECTIVITY_BLUETOOTH_HCI_VENDOR_MARVELL_TESTS_BT_HCI_MOCK_SDIO_TEST_H_

#include <fuchsia/hardware/sdio/cpp/banjo-mock.h>

#include <zxtest/zxtest.h>

#include "src/connectivity/bluetooth/hci/vendor/marvell/bt_hci_marvell.h"
#include "src/connectivity/bluetooth/hci/vendor/marvell/host_channel.h"
#include "src/connectivity/bluetooth/hci/vendor/marvell/marvell_frame.emb.h"
#include "src/connectivity/bluetooth/hci/vendor/marvell/tests/bt_hci_mock_sdio.h"
#include "src/connectivity/bluetooth/hci/vendor/marvell/tests/test_frame.h"
#include "src/devices/testing/mock-ddk/mock-device.h"

namespace bt_hci_marvell {

class BtHciMockSdioTest : public zxtest::Test {
 public:
  // 88W8987
  static constexpr uint32_t kSimProductId = 0x914a;

  static constexpr uint8_t kFakeMacAddr[6] = {0x45, 0x6d, 0x6d, 0x65, 0x74, 0x74};

  // The value we'll return for reads from the ioport address registers
  static constexpr uint32_t kSimIoportAddr = 0x4a4d43;
  static constexpr uint8_t kSimIoportAddrLow = kSimIoportAddr & 0xff;
  static constexpr uint8_t kSimIoportAddrMid = (kSimIoportAddr & 0xff00) >> CHAR_BIT;
  static constexpr uint8_t kSimIoportAddrHigh = (kSimIoportAddr & 0xff0000) >> (CHAR_BIT * 2);

  // Go through all of the steps to set up our test environment: create our two devices (SDIO fake
  // and our dut) and driver interfaces (SDIO fake and our own driver). Performs the simulated bind
  // and calls DdkInit().
  void SetUp() override;

  // Deconstruct the mock device tree
  void TearDown() override;

  // Invoke ddkInit() and verify success
  void Init();

  // Prepare for all of the sdio calls associated with enabling interrupts
  void ExpectEnableInterrupts();

  // Prepare for the controller to receive a SetMacAddr frame
  void ExpectSetMacAddr();

  // Trigger an interrupt for a SetMacAddr command complete event
  void SendSetMacAddrCompleteInterrupt();

  // Create zx::channels and make the appropriate ddk calls to open the channels for communication
  // with the driver.
  void EstablishAllChannels();

  // Write a frame to the specified host channel. If |expected_in_controller| is set, we should see
  // the frame passed to the controller immediately.
  void WriteToChannel(const TestFrame& frame_in, bool expect_in_controller);

  // Wait for a frame to arrive in the corresponding host channel.
  void ExpectFrameInHostChannel(const TestFrame& frame);

  // Simulate an interrupt event. If the "Ready to Send" flag is set, the controller can expect to
  // receive a |frame_from_host|, if one is pending. If the "Packet Available" flag is set, we
  // will queue |frame_from_controller| in the appropriate host channel.
  void SendInterruptAndProcessFrames(const uint8_t interrupt_flags,
                                     const TestFrame* frame_from_controller,
                                     const TestFrame* frame_from_host);

 protected:
  BtHciMarvell* driver_instance_;

  std::optional<DeviceOracle> device_oracle_;

  // The root of our MockDevice tree
  std::shared_ptr<MockDevice> fake_parent_ = MockDevice::FakeRootParent();

  // This is the object that will handle all of our SDIO calls to the fake parent
  BtHciMockSdio mock_sdio_device_;

  // Host-side channel handles
  zx::channel command_channel_;
  zx::channel acl_data_channel_;
  zx::channel sco_channel_;

  // Just frames that we want to keep around until the test completes
  std::list<TestFrame> frames_;

 private:
  void UnbindAndRelease();

  // The Marvell controller uses an interesting size calculation when providing a frame to the host.
  // Specifically, the read size should be (rx_len << rx_unit) rounded up to the nearest SDIO block
  // size. This function calculates reasonable values for the rx_unit and rx_len registers for a
  // given frame size. Note that there are almost always multiple values that are "correct," so
  // we just use a simple formula that uses the smallest rx_unit value possible.
  static void CalculateUnitAndLength(uint32_t frame_size, uint8_t* rx_unit, uint8_t* rx_len);

  MockDevice* dut_;
  zx::interrupt sdio_interrupt_;
};

}  // namespace bt_hci_marvell

#endif  // SRC_CONNECTIVITY_BLUETOOTH_HCI_VENDOR_MARVELL_TESTS_BT_HCI_MOCK_SDIO_TEST_H_
