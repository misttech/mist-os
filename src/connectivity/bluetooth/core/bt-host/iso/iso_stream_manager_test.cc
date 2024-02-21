// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/iso/iso_stream_manager.h"

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/testing/controller_test.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/testing/mock_controller.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/testing/test_packets.h"

namespace bt::iso {

// Connection handles, must be in the range [0x0000, 0x0eff]
constexpr hci_spec::ConnectionHandle kAclConnectionHandleId1 = 0x123;
constexpr hci_spec::ConnectionHandle kAclConnectionHandleId2 = 0x222;
constexpr hci_spec::ConnectionHandle kCisHandleId = 0xe09;

constexpr hci_spec::CigIdentifier kCigId = 0x11;
constexpr hci_spec::CisIdentifier kCisId = 0x18;

using MockControllerTestBase =
    bt::testing::FakeDispatcherControllerTest<bt::testing::MockController>;

class IsoStreamManagerTest : public MockControllerTestBase {
 public:
  IsoStreamManagerTest() = default;
  ~IsoStreamManagerTest() override = default;

  void SetUp() override {
    MockControllerTestBase::SetUp();
    iso_stream_manager_ = std::make_unique<IsoStreamManager>(
        kAclConnectionHandleId1, cmd_channel()->AsWeakPtr());
  }

 private:
  std::unique_ptr<IsoStreamManager> iso_stream_manager_;
};

// Verify that we ignore a CIS request whose ACL connection handle doesn't match
// ours.
TEST_F(IsoStreamManagerTest, IgnoreIncomingWrongConnection) {
  DynamicByteBuffer request_packet = testing::LECISRequestEventPacket(
      kAclConnectionHandleId2, kCisHandleId, kCigId, kCisId);
  test_device()->SendCommandChannelPacket(request_packet);
}

// Verify that we reject a CIS request whose ACL connection handle matches ours.
TEST_F(IsoStreamManagerTest, RejectIncomingConnection) {
  const auto le_reject_cis_packet = testing::LERejectCISRequestCommandPacket(
      kCisHandleId, pw::bluetooth::emboss::StatusCode::UNSPECIFIED_ERROR);
  EXPECT_CMD_PACKET_OUT(test_device(), le_reject_cis_packet);

  DynamicByteBuffer request_packet = testing::LECISRequestEventPacket(
      kAclConnectionHandleId1, kCisHandleId, kCigId, kCisId);
  test_device()->SendCommandChannelPacket(request_packet);
}

}  // namespace bt::iso
