// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "fidl_controller.h"

#include <lib/fidl/cpp/binding.h>

#include <optional>

#include "gmock/gmock.h"
#include "src/connectivity/bluetooth/core/bt-host/fidl/fake_vendor_server.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/common/byte_buffer.h"
#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/testing/test_helpers.h"
#include "src/lib/testing/loop_fixture/test_loop_fixture.h"

namespace bt::controllers {

namespace fhbt = fuchsia::hardware::bluetooth;

constexpr hci_spec::ConnectionHandle kConnectionHandle = 0x0001;
const StaticByteBuffer kSetAclPriorityNormalCommand(0x00);
const StaticByteBuffer kSetAclPrioritySourceCommand(0x01);
const StaticByteBuffer kSetAclPrioritySinkCommand(0x02);

class FidlControllerTest : public ::gtest::TestLoopFixture {
 public:
  void SetUp() {
    fhbt::VendorHandle vendor;

    fake_vendor_server_.emplace(vendor.NewRequest(), dispatcher());
    fidl_controller_.emplace(std::move(vendor), dispatcher());
  }

  void InitializeController() {
    controller()->Initialize(
        [&](pw::Status cb_complete_status) { complete_status_ = cb_complete_status; },
        [&](pw::Status cb_error) { controller_error_ = cb_error; });
    ASSERT_FALSE(complete_status_.has_value());
    ASSERT_FALSE(controller_error_.has_value());
  }

  FidlController* controller() { return &fidl_controller_.value(); }

  fidl::testing::FakeHciServer* hci_server() { return fake_vendor_server_->hci_server(); }

  fidl::testing::FakeVendorServer* vendor_server() { return &fake_vendor_server_.value(); }

  std::optional<pw::Status> complete_status() const { return complete_status_; }

  std::optional<pw::Status> controller_error() const { return controller_error_; }

  fhbt::BtVendorFeatures FeaturesBitsToVendorFeatures(FidlController::FeaturesBits bits) {
    fhbt::BtVendorFeatures out{0};
    if (bits & FidlController::FeaturesBits::kSetAclPriorityCommand) {
      out |= fhbt::BtVendorFeatures::SET_ACL_PRIORITY_COMMAND;
    }
    if (bits & FidlController::FeaturesBits::kAndroidVendorExtensions) {
      out |= fhbt::BtVendorFeatures::ANDROID_VENDOR_EXTENSIONS;
    }
    return out;
  }

 private:
  std::optional<pw::Status> complete_status_;
  std::optional<pw::Status> controller_error_;
  std::optional<fidl::testing::FakeVendorServer> fake_vendor_server_;
  std::optional<FidlController> fidl_controller_;
};

TEST_F(FidlControllerTest, SendAndReceiveAclPackets) {
  RETURN_IF_FATAL(InitializeController());
  RunLoopUntilIdle();
  ASSERT_THAT(complete_status(), ::testing::Optional(PW_STATUS_OK));

  const StaticByteBuffer acl_packet_0(0x00, 0x01, 0x02, 0x03);
  controller()->SendAclData(acl_packet_0.subspan());
  RunLoopUntilIdle();
  ASSERT_EQ(hci_server()->acl_packets_received().size(), 1u);
  EXPECT_THAT(hci_server()->acl_packets_received()[0], BufferEq(acl_packet_0));

  const StaticByteBuffer acl_packet_1(0x04, 0x05, 0x06, 0x07);
  controller()->SendAclData(acl_packet_1.subspan());
  RunLoopUntilIdle();
  ASSERT_EQ(hci_server()->acl_packets_received().size(), 2u);
  EXPECT_THAT(hci_server()->acl_packets_received()[1], BufferEq(acl_packet_1));

  std::vector<DynamicByteBuffer> received_acl;
  controller()->SetReceiveAclFunction([&](pw::span<const std::byte> buffer) {
    received_acl.emplace_back(BufferView(buffer.data(), buffer.size()));
  });

  hci_server()->SendAcl(acl_packet_0.view());
  RunLoopUntilIdle();
  ASSERT_EQ(received_acl.size(), 1u);
  EXPECT_THAT(received_acl[0], BufferEq(acl_packet_0));

  hci_server()->SendAcl(acl_packet_1.view());
  RunLoopUntilIdle();
  ASSERT_EQ(received_acl.size(), 2u);
  EXPECT_THAT(received_acl[1], BufferEq(acl_packet_1));

  std::optional<pw::Status> close_status;
  controller()->Close([&](pw::Status status) { close_status = status; });
  ASSERT_TRUE(close_status.has_value());
  EXPECT_EQ(close_status.value(), PW_STATUS_OK);
}

TEST_F(FidlControllerTest, SendCommandsAndReceiveEvents) {
  RETURN_IF_FATAL(InitializeController());
  RunLoopUntilIdle();
  ASSERT_THAT(complete_status(), ::testing::Optional(PW_STATUS_OK));

  const StaticByteBuffer packet_0(0x00, 0x01, 0x02, 0x03);
  controller()->SendCommand(packet_0.subspan());
  RunLoopUntilIdle();
  ASSERT_EQ(hci_server()->commands_received().size(), 1u);
  EXPECT_THAT(hci_server()->commands_received()[0], BufferEq(packet_0));

  const StaticByteBuffer packet_1(0x04, 0x05, 0x06, 0x07);
  controller()->SendCommand(packet_1.subspan());
  RunLoopUntilIdle();
  ASSERT_EQ(hci_server()->commands_received().size(), 2u);
  EXPECT_THAT(hci_server()->commands_received()[1], BufferEq(packet_1));

  std::vector<DynamicByteBuffer> events;
  controller()->SetEventFunction([&](pw::span<const std::byte> buffer) {
    events.emplace_back(BufferView(buffer.data(), buffer.size()));
  });

  hci_server()->SendEvent(packet_1.view());
  RunLoopUntilIdle();
  ASSERT_EQ(events.size(), 1u);
  EXPECT_THAT(events[0], BufferEq(packet_1));

  hci_server()->SendEvent(packet_1.view());
  RunLoopUntilIdle();
  ASSERT_EQ(events.size(), 2u);
  EXPECT_THAT(events[1], BufferEq(packet_1));

  std::optional<pw::Status> close_status;
  controller()->Close([&](pw::Status status) { close_status = status; });
  ASSERT_TRUE(close_status.has_value());
  EXPECT_EQ(close_status.value(), PW_STATUS_OK);
}

TEST_F(FidlControllerTest, CloseClosesChannels) {
  RETURN_IF_FATAL(InitializeController());
  RunLoopUntilIdle();
  ASSERT_THAT(complete_status(), ::testing::Optional(PW_STATUS_OK));

  std::optional<pw::Status> close_status;
  controller()->Close([&](pw::Status status) { close_status = status; });
  RunLoopUntilIdle();
  ASSERT_TRUE(close_status.has_value());
  EXPECT_EQ(close_status.value(), PW_STATUS_OK);
  EXPECT_FALSE(hci_server()->acl_channel_valid());
  EXPECT_FALSE(hci_server()->command_channel_valid());
}

TEST_F(FidlControllerTest, HciServerClosesChannel) {
  RETURN_IF_FATAL(InitializeController());
  RunLoopUntilIdle();
  ASSERT_THAT(complete_status(), ::testing::Optional(PW_STATUS_OK));

  EXPECT_TRUE(hci_server()->CloseAclChannel());
  RunLoopUntilIdle();
  ASSERT_THAT(controller_error(), ::testing::Optional(pw::Status::Unavailable()));

  std::optional<pw::Status> close_status;
  controller()->Close([&](pw::Status status) { close_status = status; });
  ASSERT_THAT(close_status, ::testing::Optional(PW_STATUS_OK));
}

TEST_F(FidlControllerTest, HciServerClosesProtocol) {
  RETURN_IF_FATAL(InitializeController());
  RunLoopUntilIdle();
  ASSERT_THAT(complete_status(), ::testing::Optional(PW_STATUS_OK));

  hci_server()->Unbind();
  RunLoopUntilIdle();
  ASSERT_THAT(controller_error(), ::testing::Optional(pw::Status::Unavailable()));
}

TEST_F(FidlControllerTest, VendorGetFeatures) {
  RETURN_IF_FATAL(InitializeController());
  RunLoopUntilIdle();
  ASSERT_THAT(complete_status(), ::testing::Optional(PW_STATUS_OK));

  std::optional<fhbt::BtVendorFeatures> features;
  controller()->GetFeatures(
      [&](FidlController::FeaturesBits bits) { features = FeaturesBitsToVendorFeatures(bits); });
  RunLoopUntilIdle();
  ASSERT_TRUE(features.has_value());
  EXPECT_EQ(features.value(), fhbt::BtVendorFeatures::SET_ACL_PRIORITY_COMMAND);

  std::optional<pw::Status> close_status;
  controller()->Close([&](pw::Status status) { close_status = status; });
  ASSERT_TRUE(close_status.has_value());
  EXPECT_EQ(close_status.value(), PW_STATUS_OK);
}

TEST_F(FidlControllerTest, VendorEncodeSetAclPriorityCommandNormal) {
  RETURN_IF_FATAL(InitializeController());
  RunLoopUntilIdle();
  ASSERT_THAT(complete_status(), ::testing::Optional(PW_STATUS_OK));

  pw::bluetooth::SetAclPriorityCommandParameters params;
  params.connection_handle = kConnectionHandle;
  params.priority = pw::bluetooth::AclPriority::kNormal;

  std::optional<DynamicByteBuffer> buffer;
  controller()->EncodeVendorCommand(params, [&](pw::Result<pw::span<const std::byte>> result) {
    ASSERT_TRUE(result.ok());
    buffer.emplace(BufferView(result.value().data(), result.value().size()));
  });
  RunLoopUntilIdle();
  ASSERT_TRUE(buffer);
  EXPECT_THAT(*buffer, BufferEq(kSetAclPriorityNormalCommand));
}

TEST_F(FidlControllerTest, VendorEncodeSetAclPriorityCommandSink) {
  RETURN_IF_FATAL(InitializeController());
  RunLoopUntilIdle();
  ASSERT_THAT(complete_status(), ::testing::Optional(PW_STATUS_OK));

  pw::bluetooth::SetAclPriorityCommandParameters params;
  params.connection_handle = kConnectionHandle;
  params.priority = pw::bluetooth::AclPriority::kSink;

  std::optional<DynamicByteBuffer> buffer;
  controller()->EncodeVendorCommand(params, [&](pw::Result<pw::span<const std::byte>> result) {
    ASSERT_TRUE(result.ok());
    buffer.emplace(BufferView(result.value().data(), result.value().size()));
  });
  RunLoopUntilIdle();
  ASSERT_TRUE(buffer);
  EXPECT_THAT(*buffer, BufferEq(kSetAclPrioritySinkCommand));
}

TEST_F(FidlControllerTest, VendorEncodeSetAclPriorityCommandSource) {
  RETURN_IF_FATAL(InitializeController());
  RunLoopUntilIdle();
  ASSERT_THAT(complete_status(), ::testing::Optional(PW_STATUS_OK));

  pw::bluetooth::SetAclPriorityCommandParameters params;
  params.connection_handle = kConnectionHandle;
  params.priority = pw::bluetooth::AclPriority::kSource;

  std::optional<DynamicByteBuffer> buffer;
  controller()->EncodeVendorCommand(params, [&](pw::Result<pw::span<const std::byte>> result) {
    ASSERT_TRUE(result.ok());
    buffer.emplace(BufferView(result.value().data(), result.value().size()));
  });
  RunLoopUntilIdle();
  ASSERT_TRUE(buffer);
  EXPECT_THAT(*buffer, BufferEq(kSetAclPrioritySourceCommand));
}

TEST_F(FidlControllerTest, VendorServerClosesChannelBeforeOpenHci) {
  // Loop is not run after initialization to ensure OpenHci() is not called
  RETURN_IF_FATAL(InitializeController());
  ASSERT_THAT(complete_status(), std::nullopt);
  ASSERT_THAT(controller_error(), std::nullopt);

  vendor_server()->Unbind();
  RunLoopUntilIdle();
  ASSERT_THAT(complete_status(), ::testing::Optional(pw::Status::Unavailable()));
  ASSERT_THAT(controller_error(), std::nullopt);
}

TEST_F(FidlControllerTest, VendorServerClosesProtocolBeforeInitialize) {
  vendor_server()->Unbind();
  RunLoopUntilIdle();

  RETURN_IF_FATAL(InitializeController());
  RunLoopUntilIdle();
  ASSERT_THAT(complete_status(), ::testing::Optional(pw::Status::Unavailable()));
  ASSERT_THAT(controller_error(), std::nullopt);
}

TEST_F(FidlControllerTest, VendorOpenHciError) {
  // Make OpenHci() return error during controller initialization
  vendor_server()->set_open_hci_error(true);

  RETURN_IF_FATAL(InitializeController());
  RunLoopUntilIdle();
  ASSERT_THAT(complete_status(), ::testing::Optional(pw::Status::Internal()));
  ASSERT_THAT(controller_error(), std::nullopt);
}

TEST_F(FidlControllerTest, VendorServerClosesProtocol) {
  RETURN_IF_FATAL(InitializeController());
  RunLoopUntilIdle();
  ASSERT_THAT(complete_status(), ::testing::Optional(PW_STATUS_OK));

  vendor_server()->Unbind();
  RunLoopUntilIdle();
  ASSERT_THAT(controller_error(), ::testing::Optional(pw::Status::Unavailable()));
}

}  // namespace bt::controllers
