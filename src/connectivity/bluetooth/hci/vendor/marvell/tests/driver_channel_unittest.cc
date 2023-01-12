// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/connectivity/bluetooth/hci/vendor/marvell/host_channel.h"
#include "src/connectivity/bluetooth/hci/vendor/marvell/tests/bt_hci_mock_sdio_test.h"
#include "src/connectivity/bluetooth/hci/vendor/marvell/tests/test_frame.h"

namespace bt_hci_marvell {

// Verify that attempts to open an already-open channel fail.
TEST_F(BtHciMockSdioTest, SimpleChannelOpenTest) {
  EstablishAllChannels();

  zx::channel host_cmd_ch, driver_cmd_ch;
  ASSERT_OK(zx::channel::create(/* options */ 0, &host_cmd_ch, &driver_cmd_ch));
  EXPECT_NOT_OK(driver_instance_->BtHciOpenCommandChannel(std::move(driver_cmd_ch)));

  zx::channel host_acl_ch, driver_acl_ch;
  ASSERT_OK(zx::channel::create(/* options */ 0, &host_acl_ch, &driver_acl_ch));
  EXPECT_NOT_OK(driver_instance_->BtHciOpenAclDataChannel(std::move(driver_acl_ch)));

  zx::channel host_sco_ch, driver_sco_ch;
  ASSERT_OK(zx::channel::create(/* options */ 0, &host_sco_ch, &driver_sco_ch));
  EXPECT_NOT_OK(driver_instance_->BtHciOpenScoChannel(std::move(driver_sco_ch)));
}

// General testing of frame management between host and controller
TEST_F(BtHciMockSdioTest, ItsAllPipes) {
  const TestFrame kFrameCommand1(12, ControllerChannelId::kChannelCommand, device_oracle_);
  const TestFrame kFrameAcl1(235, ControllerChannelId::kChannelAclData, device_oracle_);
  const TestFrame kFrameAcl2(499, ControllerChannelId::kChannelAclData, device_oracle_);
  const TestFrame kFrameAcl3(77, ControllerChannelId::kChannelAclData, device_oracle_);
  const TestFrame kFrameSco1(64, ControllerChannelId::kChannelScoData, device_oracle_);
  const TestFrame kFrameSco2(49, ControllerChannelId::kChannelScoData, device_oracle_);
  const TestFrame kFrameEvent1(44, ControllerChannelId::kChannelEvent, device_oracle_);

  EstablishAllChannels();

  // First frame should be immediately passed to the controller.
  WriteToChannel(kFrameCommand1, /* expect_in_controller */ true);

  // We do not expect either of these frames to be written out to the SDIO bus, since the driver
  // hasn't been sent the ready-to-receive interrupt from the mock SDIO device.
  WriteToChannel(kFrameAcl1, /* expect_in_controller */ false);
  WriteToChannel(kFrameAcl2, /* expect_in_controller */ false);

  // Controller is now ready to receive the next packet. We should receive them in the order they
  // were written, as long as they are in the same channel (unfortunately, that same guarantee does
  // not hold if they were written to two different channels).
  SendInterruptAndProcessFrames(kInterruptMaskReadyToSend, nullptr, &kFrameAcl1);
  SendInterruptAndProcessFrames(kInterruptMaskReadyToSend, nullptr, &kFrameAcl2);

  // Verify that we can send a frame over the SCO data channel
  WriteToChannel(kFrameSco1, /* expect_in_controller */ false);

  // Controller has a frame for us (kFrameEvent1) and is also ready to receive another frame from us
  SendInterruptAndProcessFrames(kInterruptMaskReadyToSend | kInterruptMaskPacketAvailable,
                                &kFrameEvent1, &kFrameSco1);
  ExpectFrameInHostChannel(kFrameEvent1);

  // Send another frame to the controller. Since the controller isn't ready for another frame, it
  // will stay in the channel.
  WriteToChannel(kFrameSco2, /* expect_in_controller */ false);

  SendInterruptAndProcessFrames(kInterruptMaskPacketAvailable, &kFrameAcl3, nullptr);
  ExpectFrameInHostChannel(kFrameAcl3);

  // Retrieve our last pending frame
  SendInterruptAndProcessFrames(kInterruptMaskReadyToSend, nullptr, &kFrameSco2);

  // No more frames pending
  SendInterruptAndProcessFrames(kInterruptMaskReadyToSend, nullptr, nullptr);
}

// Verify that frames queued in the host channels are properly passed to the controller in order.
// Note that we don't try to enqueue to more than one channel at a time, since the order in which we
// read from host channels isn't guaranteed.
TEST_F(BtHciMockSdioTest, HostToController) {
  EstablishAllChannels();

  // Accommodate a header + don't send an empty frame
  const size_t kNumPacketsToQueue = kMarvellMaxRxFrameSize - 5;

  const size_t kNumChannelsUnderTest = 3;
  const struct {
    zx::channel* channel;
    ControllerChannelId channel_id;
  } kAllChannels[kNumChannelsUnderTest] = {
      {&command_channel_, ControllerChannelId::kChannelCommand},
      {&acl_data_channel_, ControllerChannelId::kChannelAclData},
      {&sco_channel_, ControllerChannelId::kChannelScoData}};

  // Send the first frame so that the controller will block on an interrupt before sending any more.
  const TestFrame kFirstFrame(42, ControllerChannelId::kChannelCommand, device_oracle_);
  WriteToChannel(kFirstFrame, /* expect_in_controller */ true);

  // Iteratively fill up and then drain each of the host channels.
  std::list<const TestFrame> frames;
  for (size_t channel_ndx = 0; channel_ndx < kNumChannelsUnderTest; channel_ndx++) {
    for (size_t packet_ndx = 0; packet_ndx < kNumPacketsToQueue; packet_ndx++) {
      frames.emplace_back(/* size */ packet_ndx + 1, kAllChannels[channel_ndx].channel_id,
                          device_oracle_);
      WriteToChannel(frames.back(), /* expect_in_controller */ false);
    }
    for (size_t packet_ndx = 0; packet_ndx < kNumPacketsToQueue; packet_ndx++) {
      SendInterruptAndProcessFrames(kInterruptMaskReadyToSend, nullptr, &(frames.front()));
      frames.pop_front();
    }
  }
}

// Verify that frames sent by the controller are queued in order in the appropriate host channel.
TEST_F(BtHciMockSdioTest, ControllerToHost) {
  EstablishAllChannels();

  // Accommodate a header + don't send an empty frame
  const size_t kNumPacketsToQueue = kMarvellMaxRxFrameSize - 5;

  const size_t kNumChannelsUnderTest = 3;
  const struct {
    ControllerChannelId channel_id;
    zx::channel* channel;
  } kAllChannels[kNumChannelsUnderTest] = {
      {ControllerChannelId::kChannelEvent, &command_channel_},
      {ControllerChannelId::kChannelAclData, &acl_data_channel_},
      {ControllerChannelId::kChannelScoData, &sco_channel_}};

  std::list<const TestFrame> frames;
  for (size_t channel_ndx = 0; channel_ndx < kNumChannelsUnderTest; channel_ndx++) {
    for (size_t packet_ndx = 0; packet_ndx < kNumPacketsToQueue; packet_ndx++) {
      frames.emplace_back(/* size */ packet_ndx + 1, kAllChannels[channel_ndx].channel_id,
                          device_oracle_);
      SendInterruptAndProcessFrames(kInterruptMaskPacketAvailable, &(frames.back()), nullptr);
    }
  }
  for (size_t channel_ndx = 0; channel_ndx < kNumChannelsUnderTest; channel_ndx++) {
    for (size_t packet_ndx = 1; packet_ndx < kNumPacketsToQueue; packet_ndx++) {
      ExpectFrameInHostChannel(frames.front());
      frames.pop_front();
    }
  }
}

}  // namespace bt_hci_marvell
