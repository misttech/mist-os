// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/transport/iso_data_channel.h"

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/testing/controller_test.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/testing/mock_controller.h"

namespace bt::hci {

const DataBufferInfo kDefaultIsoBufferInfo(/*max_data_length=*/128,
                                           /*max_num_packets=*/4);

using TestBase = testing::FakeDispatcherControllerTest<testing::MockController>;

class IsoDataChannelTests : public TestBase {
 public:
  void SetUp() override {
    TestBase::SetUp(pw::bluetooth::Controller::FeaturesBits::kHciIso);
    ASSERT_TRUE(transport()->InitializeIsoDataChannel(kDefaultIsoBufferInfo));
  }

  IsoDataChannel* iso_data_channel() { return transport()->iso_data_channel(); }
};

// Placeholder (for now)
class IsoMockConnectionInterface : public IsoDataChannel::ConnectionInterface {
 public:
  IsoMockConnectionInterface() : weak_self_(this) {}
  ~IsoMockConnectionInterface() override = default;

  void ReceiveInboundPacket() override {}

  using WeakPtr = WeakSelf<IsoMockConnectionInterface>::WeakPtr;
  IsoMockConnectionInterface::WeakPtr GetWeakPtr() {
    return weak_self_.GetWeakPtr();
  }

 private:
  WeakSelf<IsoMockConnectionInterface> weak_self_;
};

// Verify that we can register and unregister connections
TEST_F(IsoDataChannelTests, RegisterConnections) {
  ASSERT_NE(iso_data_channel(), nullptr);
  IsoMockConnectionInterface mock_iface;
  constexpr hci_spec::ConnectionHandle kIsoHandle1 = 0x123;
  EXPECT_TRUE(iso_data_channel()->RegisterConnection(kIsoHandle1,
                                                     mock_iface.GetWeakPtr()));

  constexpr hci_spec::ConnectionHandle kIsoHandle2 = 0x456;
  EXPECT_TRUE(iso_data_channel()->RegisterConnection(kIsoHandle2,
                                                     mock_iface.GetWeakPtr()));

  // Attempt to re-register a handle fails
  EXPECT_FALSE(iso_data_channel()->RegisterConnection(kIsoHandle1,
                                                      mock_iface.GetWeakPtr()));

  // Can unregister connections that were previously registered
  EXPECT_TRUE(iso_data_channel()->UnregisterConnection(kIsoHandle2));
  EXPECT_TRUE(iso_data_channel()->UnregisterConnection(kIsoHandle1));

  // Cannot unregister connections that never been registered, or that have
  // already been unregistered
  constexpr hci_spec::ConnectionHandle kIsoHandle3 = 0x789;
  EXPECT_FALSE(iso_data_channel()->UnregisterConnection(kIsoHandle3));
  EXPECT_FALSE(iso_data_channel()->UnregisterConnection(kIsoHandle2));
  EXPECT_FALSE(iso_data_channel()->UnregisterConnection(kIsoHandle1));
}

}  // namespace bt::hci
