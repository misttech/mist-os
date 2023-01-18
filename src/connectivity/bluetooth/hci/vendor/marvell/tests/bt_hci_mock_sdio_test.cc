// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/connectivity/bluetooth/hci/vendor/marvell/tests/bt_hci_mock_sdio_test.h"

#include <fuchsia/hardware/bt/hci/c/banjo.h>
#include <lib/ddk/metadata.h>

// Validate that an SDIO read/write operation matches our expectations. This definition has to be
// provided when using ddk::MockSdio.
bool operator==(const sdio_rw_txn_t& actual, const sdio_rw_txn_t& expected) {
  if ((actual.addr != expected.addr) || (actual.incr != expected.incr) ||
      (actual.write != expected.write)) {
    return false;
  }

  // These aren't hard requirements of the implementation, but it's very unlikely they would change.
  ZX_ASSERT(actual.buffers_count == 1);
  ZX_ASSERT(expected.buffers_count == 1);
  ZX_ASSERT(actual.buffers_list[0].type == SDMMC_BUFFER_TYPE_VMO_HANDLE);
  ZX_ASSERT(expected.buffers_list[0].type == SDMMC_BUFFER_TYPE_VMO_HANDLE);

  const sdmmc_buffer_region* actual_region = &actual.buffers_list[0];
  const sdmmc_buffer_region* expected_region = &expected.buffers_list[0];
  if (actual_region->size != expected_region->size) {
    return false;
  }

  size_t frame_size = actual_region->size;
  uint8_t expected_data[frame_size];
  if (zx_vmo_read(expected_region->buffer.vmo, expected_data, expected_region->offset,
                  frame_size) != ZX_OK) {
    return false;
  }

  if (actual.write) {
    uint8_t actual_data[frame_size];
    if (zx_vmo_read(actual_region->buffer.vmo, actual_data, actual_region->offset, frame_size) !=
        ZX_OK) {
      return false;
    }
    if (memcmp(actual_data, expected_data, frame_size) != 0) {
      return false;
    }
  } else {
    // It's a bit counter-intuitive to perform the vmo write in the equality operator, however this
    // is the point where we have all the data we need to copy the values over (and it has a certain
    // symmetry since it's also where we perform the vmo read. An alternative approach would be to
    // override SdioDoRwTxn(), however that function doesn't have straightforward access to the
    // expected data and would have to reach into the mock function implementation to get it.
    if (zx_vmo_write(actual_region->buffer.vmo, expected_data, actual_region->offset, frame_size) !=
        ZX_OK) {
      return false;
    }
    // There is no frame validation here because the caller only passed us a location to store data
    // into.
  }
  return true;
}

namespace bt_hci_marvell {

void BtHciMockSdioTest::SetUp() {
  zx::result<DeviceOracle> result = DeviceOracle::Create(kSimProductId);
  ASSERT_TRUE(result.is_ok());
  device_oracle_ = result.value();

  zxtest::Test::SetUp();

  // Construct the root of our mock device tree
  const sdio_protocol_t* sdio_proto = mock_sdio_device_.GetProto();
  fake_parent_->AddProtocol(ZX_PROTOCOL_SDIO, sdio_proto->ops, sdio_proto->ctx);

  // Prepare for hardware info query
  sdio_hw_info_t hw_info;
  hw_info.func_hw_info.product_id = kSimProductId;
  mock_sdio_device_.ExpectGetDevHwInfo(ZX_OK, hw_info);

  // Prepare for metadata query
  fake_parent_->SetMetadata(DEVICE_METADATA_MAC_ADDRESS, &kFakeMacAddr, sizeof(kFakeMacAddr));

  // Create a virtual interrupt (and duplicate) for use by the driver
  ASSERT_OK(zx::interrupt::create(zx::resource(), 0, ZX_INTERRUPT_VIRTUAL, &sdio_interrupt_));
  zx::interrupt interrupt_dup;
  ASSERT_OK(sdio_interrupt_.duplicate(ZX_RIGHT_SAME_RIGHTS, &interrupt_dup));
  mock_sdio_device_.ExpectGetInBandIntr(ZX_OK, std::move(interrupt_dup));

  // Enable bus
  mock_sdio_device_.ExpectEnableFn(ZX_OK);

  // Set block size
  uint16_t expected_block_size = device_oracle_->GetSdioBlockSize();
  mock_sdio_device_.ExpectUpdateBlockSize(ZX_OK, expected_block_size, /* deflt */ false);

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
      .ExpectDoRwByte(ZX_OK, /* write */ false, ioport_addr, /* write_byte */ 0, kSimIoportAddrLow)
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

  ExpectEnableInterrupts();

  ExpectSetMacAddr();

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

void BtHciMockSdioTest::TearDown() {
  UnbindAndRelease();
  zxtest::Test::TearDown();

  // Verify that the expected calls into the mock SDIO API were made
  ASSERT_NO_FATAL_FAILURE(mock_sdio_device_.VerifyAndClear());
}

void BtHciMockSdioTest::Init() {
  // Invoke ddkInit()
  dut_->InitOp();

  // Wait for the SetMacAddr block write to complete and then send the corresponding command
  // complete event.
  mock_sdio_device_.block_write_complete_.Wait();
  SendSetMacAddrCompleteInterrupt();

  // Verify that the Init operation returned a completion status
  ASSERT_EQ(ZX_OK, dut_->WaitUntilInitReplyCalled());

  // Verify that the status was success
  ASSERT_EQ(ZX_OK, dut_->InitReplyCallStatus());
}

void BtHciMockSdioTest::ExpectEnableInterrupts() {
  // Check that the interrupt mask bits are set as expected
  uint32_t intr_mask_addr = device_oracle_->GetRegAddrInterruptMask();
  uint8_t intr_mask_value = 0;
  mock_sdio_device_.ExpectDoRwByte(ZX_OK, /* write */ false, intr_mask_addr, /* write_byte */ 0,
                                   /* out_read_byte */ intr_mask_value);
  intr_mask_value &= ~(kInterruptMaskReadyToSend | kInterruptMaskPacketAvailable);
  intr_mask_value |= (kInterruptMaskReadyToSend | kInterruptMaskPacketAvailable);
  mock_sdio_device_.ExpectDoRwByte(ZX_OK, /* write */ true, intr_mask_addr,
                                   /* write_byte */ intr_mask_value, /* out_read_byte */ 0);

  // Verify that SDIO interrupts are re-enabled
  mock_sdio_device_.ExpectEnableFnIntr(ZX_OK);
}

void BtHciMockSdioTest::ExpectSetMacAddr() {
  std::vector<uint8_t> set_mac_addr_frame_data = {
      // HCI Header: opcode (2 bytes), parameter_total_size
      0x22, 0xfc, 0x8,
      // Vendor Header: cmd_type, cmd_length
      static_cast<uint8_t>(ControllerChannelId::kChannelVendor), 6,
      // Parameters: mac address (reversed)
      kFakeMacAddr[5], kFakeMacAddr[4], kFakeMacAddr[3], kFakeMacAddr[2], kFakeMacAddr[1],
      kFakeMacAddr[0]};
  auto frame = frames_.emplace(frames_.begin(), set_mac_addr_frame_data,
                               ControllerChannelId::kChannelVendor, device_oracle_);
  sdio_rw_txn_t txn = frame->GetSdioTxn(/* is_write */ true, kSimIoportAddr);
  mock_sdio_device_.ExpectDoRwTxn(ZX_OK, txn);
}

void BtHciMockSdioTest::SendSetMacAddrCompleteInterrupt() {
  std::vector<uint8_t> set_mac_addr_complete_frame_data = {
      // HCI Event Header: event_code, parameter_total_size
      0xe, 4,
      // Command complete event: num_hci_command_packets, command_opcode (2 bytes)
      1, 0x22, 0xfc,
      // Return parameter (success)
      0};
  auto frame = frames_.emplace(frames_.begin(), set_mac_addr_complete_frame_data,
                               ControllerChannelId::kChannelEvent, device_oracle_);
  SendInterruptAndProcessFrames(kInterruptMaskReadyToSend | kInterruptMaskPacketAvailable,
                                &(*frame), nullptr);
}

void BtHciMockSdioTest::EstablishAllChannels() {
  // Command
  zx::channel controller_command_channel;
  ASSERT_OK(zx::channel::create(/* options */ 0, &command_channel_, &controller_command_channel));
  driver_instance_->BtHciOpenCommandChannel(std::move(controller_command_channel));

  // ACL data
  zx::channel controller_acl_data_channel;
  ASSERT_OK(zx::channel::create(/* options */ 0, &acl_data_channel_, &controller_acl_data_channel));
  driver_instance_->BtHciOpenAclDataChannel(std::move(controller_acl_data_channel));

  // SCO
  zx::channel controller_sco_channel;
  ASSERT_OK(zx::channel::create(/* options */ 0, &sco_channel_, &controller_sco_channel));
  driver_instance_->BtHciOpenScoChannel(std::move(controller_sco_channel));
}

void BtHciMockSdioTest::WriteToChannel(const TestFrame& frame_in, bool expect_in_controller) {
  if (expect_in_controller) {
    sdio_rw_txn_t txn = frame_in.GetSdioTxn(/* is_write */ true, kSimIoportAddr);
    mock_sdio_device_.ExpectDoRwTxn(ZX_OK, txn);
  }
  switch (frame_in.channel_id()) {
    case ControllerChannelId::kChannelCommand:
      command_channel_.write(/* options */ 0, frame_in.data(), frame_in.size(), nullptr, 0);
      break;
    case ControllerChannelId::kChannelAclData:
      acl_data_channel_.write(/* options */ 0, frame_in.data(), frame_in.size(), nullptr, 0);
      break;
    case ControllerChannelId::kChannelScoData:
      sco_channel_.write(/* options */ 0, frame_in.data(), frame_in.size(), nullptr, 0);
      break;
    default:
      ASSERT_TRUE(0);
  }
  mock_sdio_device_.block_write_complete_.Wait();
}

void BtHciMockSdioTest::CalculateUnitAndLength(uint32_t frame_size_with_header, uint8_t* rx_unit,
                                               uint8_t* rx_len) {
  *rx_unit = 0;

  // Stop as soon as we find a value for rx_len that will fit into a single register.
  while (frame_size_with_header > UINT8_MAX) {
    (*rx_unit)++;
    bool overflow = frame_size_with_header & 0x1;
    frame_size_with_header >>= 1;
    if (overflow) {
      frame_size_with_header += 1;
    }
  }

  *rx_len = frame_size_with_header & 0xff;
}

void BtHciMockSdioTest::ExpectFrameInHostChannel(const TestFrame& frame) {
  // Wait for frame to show up in the appropriate host channel
  zx_signals_t observed_signals;
  zx::channel* out_channel;
  switch (frame.channel_id()) {
    case ControllerChannelId::kChannelEvent:
      out_channel = &command_channel_;
      break;
    case ControllerChannelId::kChannelAclData:
      out_channel = &acl_data_channel_;
      break;
    case ControllerChannelId::kChannelScoData:
      out_channel = &sco_channel_;
      break;
    default:
      ZX_ASSERT(0);
  }
  out_channel->wait_one(ZX_CHANNEL_READABLE, zx::deadline_after(zx::duration::infinite()),
                        &observed_signals);
  ASSERT_NE(observed_signals & ZX_CHANNEL_READABLE, 0);
  uint8_t received_frame[frame.size()];
  uint32_t actual_byte_count;
  ASSERT_OK(out_channel->read(/* options */ 0, received_frame, /* handles */ nullptr,
                              /* num_bytes */ frame.size(), /* num_handles */ 0,
                              /* actual_bytes */ &actual_byte_count,
                              /* actual_handles */ nullptr));
  ASSERT_EQ(actual_byte_count, frame.size());
  ASSERT_EQ(0, memcmp(frame.data(), received_frame, frame.size()));
}

// This is perhaps the most complicated function in this class, and is designed to work hand-in-hand
// with the driver. Without a full controller simulator, this is the best we can do.
void BtHciMockSdioTest::SendInterruptAndProcessFrames(const uint8_t interrupt_flags,
                                                      const TestFrame* frame_from_controller,
                                                      const TestFrame* frame_from_host) {
  bool packet_available = interrupt_flags & kInterruptMaskPacketAvailable;

  // The driver will first check the interrupt status
  uint32_t interrupt_status_reg_addr = device_oracle_->GetRegAddrInterruptStatus();
  uint8_t interrupt_status = interrupt_flags;
  mock_sdio_device_.ExpectDoRwByte(ZX_OK, /* write */ false, interrupt_status_reg_addr,
                                   /* write_byte */ 0, /* out_read_byte */ interrupt_status);

  // Simulate a frame from the controller
  if (packet_available) {
    ZX_ASSERT(frame_from_controller);

    // Report the frame size to the driver using the Marvell-specific calculation, where frame
    // size is <= (rx_len << rx_unit).
    uint8_t rx_unit, rx_len;
    uint32_t frame_size_with_header =
        frame_from_controller->size() + MarvellFrameHeaderView::SizeInBytes();
    CalculateUnitAndLength(frame_size_with_header, &rx_unit, &rx_len);
    uint32_t rx_len_reg_addr = device_oracle_->GetRegAddrRxLen();
    mock_sdio_device_.ExpectDoRwByte(ZX_OK, /* write */ false, rx_len_reg_addr,
                                     /* write_byte */ 0, /* out_read_byte */ rx_len);
    uint32_t rx_unit_reg_addr = device_oracle_->GetRegAddrRxUnit();
    mock_sdio_device_.ExpectDoRwByte(ZX_OK, /* write */ false, rx_unit_reg_addr,
                                     /* write_byte */ 0, /* out_read_byte */ rx_unit);

    // The next expected operation is the actual frame read
    sdio_rw_txn_t txn = frame_from_controller->GetSdioTxn(/* is_write */ false, kSimIoportAddr);
    mock_sdio_device_.ExpectDoRwTxn(ZX_OK, txn);
  }

  mock_sdio_device_.ExpectAckInBandIntr();

  if (frame_from_host) {
    sdio_rw_txn_t txn = frame_from_host->GetSdioTxn(/* is_write */ true, kSimIoportAddr);
    mock_sdio_device_.ExpectDoRwTxn(ZX_OK, txn);
  }

  sdio_interrupt_.trigger(/* options */ 0, zx::time());

  // Since the block write is occurring asynchronously (on the driver thread), we want to wait for
  // that operation to complete before we continue with the test.
  if (frame_from_host) {
    mock_sdio_device_.block_write_complete_.Wait();
  }

  // Similarly, if we don't wait on the interrupt ACK operation, we may end up missing interrupts
  // in later tests.
  mock_sdio_device_.ack_complete_.Wait();
}

void BtHciMockSdioTest::UnbindAndRelease() {
  // Flag our mock device for removal
  device_async_remove(dut_);

  // Remove from the mock device tree
  mock_ddk::ReleaseFlaggedDevices(fake_parent_.get());
}

}  // namespace bt_hci_marvell
