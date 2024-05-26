// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/power/drivers/fusb302/fusb302-protocol.h"

#include <fidl/fuchsia.hardware.i2c/cpp/wire.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/mock-i2c/mock-i2c.h>
#include <lib/stdcompat/span.h>
#include <lib/zx/result.h>

#include <cstdint>
#include <optional>
#include <utility>

#include <zxtest/zxtest.h>

#include "src/devices/power/drivers/fusb302/usb-pd-message-type.h"
#include "src/devices/power/drivers/fusb302/usb-pd-message.h"

namespace fusb302 {

namespace {

// Register addresses from Table 16 "Register Definitions" on page 18 of the
// Rev 5 datasheet.
constexpr int kFifosAddress = 0x43;
constexpr int kStatus0Address = 0x40;
constexpr int kStatus1Address = 0x41;

// Tokens from Table 41 "Tokens used in TxFIFO" on page 29 of the Rev 5
// datasheet.
const uint8_t kTxOnTxToken = 0xa1;
const uint8_t kSync1TxToken = 0x12;
const uint8_t kSync2TxToken = 0x13;
// const uint8_t kSync3TxToken = 0x1b;
// const uint8_t kReset1TxToken = 0x15;
// const uint8_t kReset2TxToken = 0x16;
const uint8_t kPackSymTxToken = 0x80;
const uint8_t kJamCrcTxToken = 0xff;
const uint8_t kEopTxToken = 0x14;
const uint8_t kTxOffTxToken = 0xfe;

// Tokens from Table 42 "Tokens used in RxFIFO" on page 29 of the Rev 5
// datasheet.
constexpr uint8_t kSopRxToken = 0b1110'0000;

class Fusb302ProtocolTest : public zxtest::Test {
 public:
  void SetUp() override {
    fdf::Logger::SetGlobalInstance(&logger_);
    auto endpoints = fidl::Endpoints<fuchsia_hardware_i2c::Device>::Create();
    mock_i2c_client_ = std::move(endpoints.client);

    EXPECT_OK(loop_.StartThread());
    fidl::BindServer<fuchsia_hardware_i2c::Device>(loop_.dispatcher(), std::move(endpoints.server),
                                                   &mock_i2c_);

    fifos_.emplace(mock_i2c_client_);
    protocol_.emplace(fifos_.value());
  }

  void TearDown() override {
    fdf::Logger::SetGlobalInstance(nullptr);
    mock_i2c_.VerifyAndClear();
  }

 protected:
  void MockReceiveSourceCapabilities(usb_pd::MessageId message_id) {
    mock_i2c_.ExpectWrite({kFifosAddress})
        .ExpectReadStop({kSopRxToken, 0xa1,
                         static_cast<uint8_t>(0x11 | (static_cast<uint8_t>(message_id) << 1))});
    mock_i2c_.ExpectWrite({kFifosAddress})
        .ExpectReadStop({0x2c, 0x91, 0x01, 0x27, 0xb1, 0x9b, 0x26, 0x94});
  }

  void MockReceiveGoodCrc(usb_pd::MessageId message_id) {
    mock_i2c_.ExpectWrite({kFifosAddress})
        .ExpectReadStop({kSopRxToken, 0x61,
                         static_cast<uint8_t>(0x01 | (static_cast<uint8_t>(message_id) << 1))});
    mock_i2c_.ExpectWrite({kFifosAddress}).ExpectReadStop({0x8f, 0x78, 0x38, 0x4a});
  }

  void MockReceiveSoftReset(usb_pd::MessageId message_id) {
    mock_i2c_.ExpectWrite({kFifosAddress})
        .ExpectReadStop({kSopRxToken, 0xad,
                         static_cast<uint8_t>(0x01 | (static_cast<uint8_t>(message_id) << 1))});
    mock_i2c_.ExpectWrite({kFifosAddress}).ExpectReadStop({0x8f, 0x78, 0x38, 0x4a});
  }

  void MockReceiveAccept(usb_pd::MessageId message_id) {
    mock_i2c_.ExpectWrite({kFifosAddress})
        .ExpectReadStop({kSopRxToken, 0xa3,
                         static_cast<uint8_t>(0x01 | (static_cast<uint8_t>(message_id) << 1))});
    mock_i2c_.ExpectWrite({kFifosAddress}).ExpectReadStop({0x8f, 0x78, 0x38, 0x4a});
  }

  void MockReceivePowerReady(usb_pd::MessageId message_id) {
    mock_i2c_.ExpectWrite({kFifosAddress})
        .ExpectReadStop({kSopRxToken, 0xa6,
                         static_cast<uint8_t>(0x01 | (static_cast<uint8_t>(message_id) << 1))});
    mock_i2c_.ExpectWrite({kFifosAddress}).ExpectReadStop({0x8f, 0x78, 0x38, 0x4a});
  }

  void MockReceiveFifoEmpty() { mock_i2c_.ExpectWrite({kStatus1Address}).ExpectReadStop({0x20}); }

  void MockReceiveFifoNotEmpty() {
    mock_i2c_.ExpectWrite({kStatus1Address}).ExpectReadStop({0x00});
  }

  void MockReceiveGetSinkCapabilities(usb_pd::MessageId message_id) {
    mock_i2c_.ExpectWrite({kFifosAddress})
        .ExpectReadStop({kSopRxToken, 0x68,
                         static_cast<uint8_t>(0x01 | (static_cast<uint8_t>(message_id) << 1))});
    mock_i2c_.ExpectWrite({kFifosAddress}).ExpectReadStop({0xef, 0x8f, 0x4c, 0x92});
  }

  // MockWrite*ToFifo() methods factor out the message-specific bytes. If
  // kTxOffTxToken and kTxOnTxToken are shared between a piggy-backed message
  // and its main message, they will not be covered by these methods.

  void MockWriteGoodCrcToFifo(usb_pd::MessageId message_id) {
    mock_i2c_.ExpectWriteStop({kFifosAddress, kSync1TxToken, kSync1TxToken, kSync1TxToken,
                               kSync2TxToken, kPackSymTxToken | 2, 0x41,
                               static_cast<uint8_t>(0x00 | (static_cast<uint8_t>(message_id) << 1)),
                               kJamCrcTxToken, kEopTxToken, kTxOffTxToken, kTxOnTxToken});
  }
  void MockWriteSoftResetToFifo(usb_pd::MessageId message_id) {
    mock_i2c_.ExpectWriteStop({kFifosAddress, kSync1TxToken, kSync1TxToken, kSync1TxToken,
                               kSync2TxToken, kPackSymTxToken | 2, 0x4d,
                               static_cast<uint8_t>(0x00 | static_cast<uint8_t>(message_id) << 1),
                               kJamCrcTxToken, kEopTxToken, kTxOffTxToken, kTxOnTxToken});
  }

  // The message's data must be `MockPowerRequestDataObjects()`.
  void MockWritePowerRequestToFifo(usb_pd::MessageId message_id) {
    mock_i2c_.ExpectWriteStop({kFifosAddress, kSync1TxToken, kSync1TxToken, kSync1TxToken,
                               kSync2TxToken, kPackSymTxToken | 6, 0x42,
                               static_cast<uint8_t>(0x10 | static_cast<uint8_t>(message_id) << 1),
                               0x2c, 0xb1, 0x04, 0x11, kJamCrcTxToken, kEopTxToken, kTxOffTxToken,
                               kTxOnTxToken});
  }
  static cpp20::span<const uint32_t> MockPowerRequestDataObjects() {
    static constexpr uint32_t kMockDataObjects[] = {0x1104b12c};
    return kMockDataObjects;
  }

  // The message's data must be `MockSinkCapabilitiesDataObjects()`.
  void MockWriteSinkCapabilitiesToFifo(usb_pd::MessageId message_id) {
    mock_i2c_.ExpectWriteStop(
        {kFifosAddress,  kSync1TxToken,
         kSync1TxToken,  kSync1TxToken,
         kSync2TxToken,  kPackSymTxToken | 10,
         0x44,           static_cast<uint8_t>(0x20 | static_cast<uint8_t>(message_id) << 1),
         0xe0,           0x91,
         0x01,           0x04,
         0x0a,           0xd1,
         0x02,           0x00,
         kJamCrcTxToken, kEopTxToken,
         kTxOffTxToken,  kTxOnTxToken});
  }
  static cpp20::span<const uint32_t> MockSinkCapabilitiesDataObjects() {
    static constexpr uint32_t kMockDataObjects[] = {0x040191e0, 0x0002d10a};
    return kMockDataObjects;
  }

  void MockTransmitFifoEmpty() { mock_i2c_.ExpectWrite({kStatus1Address}).ExpectReadStop({0x08}); }

  void MockNoBmcTransmissionDetected() {
    mock_i2c_.ExpectWrite({kStatus0Address}).ExpectReadStop({0x80});
  }

  fdf::Logger logger_{"fusb302-protocol-test", FUCHSIA_LOG_DEBUG, zx::socket{},
                      fidl::WireClient<fuchsia_logger::LogSink>()};

  async::Loop loop_{&kAsyncLoopConfigNeverAttachToThread};
  mock_i2c::MockI2c mock_i2c_;
  fidl::ClientEnd<fuchsia_hardware_i2c::Device> mock_i2c_client_;
  std::optional<Fusb302Fifos> fifos_;
  std::optional<Fusb302Protocol> protocol_;
};

TEST_F(Fusb302ProtocolTest, Transmit) {
  MockTransmitFifoEmpty();
  MockNoBmcTransmissionDetected();
  MockWritePowerRequestToFifo(usb_pd::MessageId(0));

  usb_pd::Message request(usb_pd::MessageType::kRequestPower, usb_pd::MessageId(0),
                          usb_pd::PowerRole::kSink, usb_pd::SpecRevision::kRev2,
                          usb_pd::DataRole::kUpstreamFacingPort, MockPowerRequestDataObjects());
  EXPECT_OK(protocol_->Transmit(request));
  EXPECT_EQ(TransmissionState::kPending, protocol_->transmission_state());
}

TEST_F(Fusb302ProtocolTest, DrainReceiveFifoOneMessage) {
  MockReceiveFifoNotEmpty();
  MockReceiveSourceCapabilities(usb_pd::MessageId(0));
  MockReceiveFifoEmpty();

  EXPECT_OK(protocol_->DrainReceiveFifo());

  ASSERT_TRUE(protocol_->HasUnreadMessage());
  EXPECT_EQ(usb_pd::MessageType::kSourceCapabilities,
            protocol_->FirstUnreadMessage().header().message_type());
  EXPECT_EQ(usb_pd::MessageId(0), protocol_->FirstUnreadMessage().header().message_id());
  ASSERT_EQ(1u, protocol_->FirstUnreadMessage().data_objects().size());
  EXPECT_EQ(0x2701912c, protocol_->FirstUnreadMessage().data_objects()[0]);
}

TEST_F(Fusb302ProtocolTest, DrainReceiveFifoOneMessageMidStream) {
  MockReceiveFifoNotEmpty();
  MockReceiveSourceCapabilities(usb_pd::MessageId(3));
  MockReceiveFifoEmpty();

  EXPECT_OK(protocol_->DrainReceiveFifo());

  ASSERT_TRUE(protocol_->HasUnreadMessage());
  EXPECT_EQ(usb_pd::MessageType::kSourceCapabilities,
            protocol_->FirstUnreadMessage().header().message_type());
  EXPECT_EQ(usb_pd::MessageId(3), protocol_->FirstUnreadMessage().header().message_id());
  ASSERT_EQ(1u, protocol_->FirstUnreadMessage().data_objects().size());
  EXPECT_EQ(0x2701912c, protocol_->FirstUnreadMessage().data_objects()[0]);
}

TEST_F(Fusb302ProtocolTest, MarkMessageAsReadAfterHardwareRepliedGoodCrc) {
  MockReceiveFifoNotEmpty();
  MockReceiveGetSinkCapabilities(usb_pd::MessageId(0));
  MockReceiveFifoEmpty();

  EXPECT_OK(protocol_->DrainReceiveFifo());
  EXPECT_TRUE(protocol_->HasUnreadMessage());

  protocol_->DidTransmitGoodCrc();

  // Not expected to generate any I2C activity.
  EXPECT_OK(protocol_->MarkMessageAsRead());
  EXPECT_FALSE(protocol_->HasUnreadMessage());

  // Verify that DidTransmitGoodCrc() / MarkMessageAsRead() did not change the
  // outgoing message flow state.
  ASSERT_EQ(TransmissionState::kSuccess, protocol_->transmission_state());
  EXPECT_EQ(usb_pd::MessageId(0), protocol_->next_transmitted_message_id());
}

TEST_F(Fusb302ProtocolTest, MarkMessageAsReadTransmitsGoodCrc) {
  MockReceiveFifoNotEmpty();
  MockReceiveSourceCapabilities(usb_pd::MessageId(0));
  MockReceiveFifoEmpty();

  MockTransmitFifoEmpty();
  MockNoBmcTransmissionDetected();
  MockWriteGoodCrcToFifo(usb_pd::MessageId(0));

  EXPECT_OK(protocol_->DrainReceiveFifo());
  EXPECT_TRUE(protocol_->HasUnreadMessage());

  EXPECT_OK(protocol_->MarkMessageAsRead());
  EXPECT_FALSE(protocol_->HasUnreadMessage());

  // Verify that DidTransmitGoodCrc() / MarkMessageAsRead() did not change the
  // outgoing message flow state.
  ASSERT_EQ(TransmissionState::kSuccess, protocol_->transmission_state());
  EXPECT_EQ(usb_pd::MessageId(0), protocol_->next_transmitted_message_id());
}

TEST_F(Fusb302ProtocolTest, DrainReceiveFifoIgnoresRepeatedMessageIdAfterSync) {
  // Get the PD protocol to sync up Message ID 4.
  {
    MockReceiveFifoNotEmpty();
    MockReceiveSourceCapabilities(usb_pd::MessageId(4));
    MockReceiveFifoEmpty();

    MockTransmitFifoEmpty();
    MockNoBmcTransmissionDetected();
    MockWriteGoodCrcToFifo(usb_pd::MessageId(4));

    EXPECT_OK(protocol_->DrainReceiveFifo());
    EXPECT_OK(protocol_->MarkMessageAsRead());
    ASSERT_FALSE(protocol_->HasUnreadMessage());
  }

  MockReceiveFifoNotEmpty();
  MockReceiveSourceCapabilities(usb_pd::MessageId(4));
  MockReceiveFifoEmpty();

  EXPECT_OK(protocol_->DrainReceiveFifo());
  EXPECT_FALSE(protocol_->HasUnreadMessage());
}

TEST_F(Fusb302ProtocolTest, DrainReceiveFifoIgnoresMismatchedMessageIdAfterSync) {
  // Get the PD protocol to sync up Message ID 4.
  {
    MockReceiveFifoNotEmpty();
    MockReceiveSourceCapabilities(usb_pd::MessageId(4));
    MockReceiveFifoEmpty();

    MockTransmitFifoEmpty();
    MockNoBmcTransmissionDetected();
    MockWriteGoodCrcToFifo(usb_pd::MessageId(4));

    EXPECT_OK(protocol_->DrainReceiveFifo());
    EXPECT_OK(protocol_->MarkMessageAsRead());
    ASSERT_FALSE(protocol_->HasUnreadMessage());
  }

  MockReceiveFifoNotEmpty();
  MockReceiveSourceCapabilities(usb_pd::MessageId(3));
  MockReceiveFifoEmpty();

  EXPECT_OK(protocol_->DrainReceiveFifo());
  EXPECT_FALSE(protocol_->HasUnreadMessage());
}

TEST_F(Fusb302ProtocolTest, DrainReceiveFifoValidMessageAfterMismatchedMessageId) {
  // Get the PD protocol to sync up Message ID 4.
  {
    MockReceiveFifoNotEmpty();
    MockReceiveSourceCapabilities(usb_pd::MessageId(4));
    MockReceiveFifoEmpty();

    MockTransmitFifoEmpty();
    MockNoBmcTransmissionDetected();
    MockWriteGoodCrcToFifo(usb_pd::MessageId(4));

    EXPECT_OK(protocol_->DrainReceiveFifo());
    ASSERT_TRUE(protocol_->HasUnreadMessage());
    EXPECT_OK(protocol_->MarkMessageAsRead());
    ASSERT_FALSE(protocol_->HasUnreadMessage());
  }

  MockReceiveFifoNotEmpty();
  MockReceiveSourceCapabilities(usb_pd::MessageId(3));
  MockReceiveFifoNotEmpty();
  MockReceiveGetSinkCapabilities(usb_pd::MessageId(5));
  MockReceiveFifoEmpty();

  EXPECT_OK(protocol_->DrainReceiveFifo());
  ASSERT_TRUE(protocol_->HasUnreadMessage());
  EXPECT_EQ(usb_pd::MessageType::kGetSinkCapabilities,
            protocol_->FirstUnreadMessage().header().message_type());
  EXPECT_EQ(usb_pd::MessageId(5), protocol_->FirstUnreadMessage().header().message_id());
}

TEST_F(Fusb302ProtocolTest, MarkMessageAsReadSendingGoodCrcUpdatesReceiveCounter) {
  MockReceiveFifoNotEmpty();
  MockReceiveSourceCapabilities(usb_pd::MessageId(0));
  MockReceiveFifoEmpty();

  MockTransmitFifoEmpty();
  MockNoBmcTransmissionDetected();
  MockWriteGoodCrcToFifo(usb_pd::MessageId(0));

  // Validates that MarkMessageAsRead() incremented the receiving MessageID.
  MockReceiveFifoNotEmpty();
  MockReceiveGetSinkCapabilities(usb_pd::MessageId(1));
  MockReceiveFifoEmpty();

  EXPECT_OK(protocol_->DrainReceiveFifo());
  EXPECT_TRUE(protocol_->HasUnreadMessage());
  EXPECT_EQ(usb_pd::MessageType::kSourceCapabilities,
            protocol_->FirstUnreadMessage().header().message_type());

  EXPECT_OK(protocol_->MarkMessageAsRead());
  EXPECT_FALSE(protocol_->HasUnreadMessage());

  EXPECT_OK(protocol_->DrainReceiveFifo());
  EXPECT_TRUE(protocol_->HasUnreadMessage());
  EXPECT_EQ(usb_pd::MessageType::kGetSinkCapabilities,
            protocol_->FirstUnreadMessage().header().message_type());
}

TEST_F(Fusb302ProtocolTest, DidTransmitGoodCrcUpdatesReceiveCounter) {
  MockReceiveFifoNotEmpty();
  MockReceiveSourceCapabilities(usb_pd::MessageId(0));
  MockReceiveFifoEmpty();

  // Validates that DidTransmitGoodCrc() or MarkMessageAsRead() incremented the
  // receiving-side MessageID.
  MockReceiveFifoNotEmpty();
  MockReceiveGetSinkCapabilities(usb_pd::MessageId(1));
  MockReceiveFifoEmpty();

  EXPECT_OK(protocol_->DrainReceiveFifo());
  EXPECT_TRUE(protocol_->HasUnreadMessage());
  EXPECT_EQ(usb_pd::MessageType::kSourceCapabilities,
            protocol_->FirstUnreadMessage().header().message_type());

  protocol_->DidTransmitGoodCrc();
  EXPECT_OK(protocol_->MarkMessageAsRead());

  EXPECT_OK(protocol_->DrainReceiveFifo());
  EXPECT_TRUE(protocol_->HasUnreadMessage());
  EXPECT_EQ(usb_pd::MessageType::kGetSinkCapabilities,
            protocol_->FirstUnreadMessage().header().message_type());
}

TEST_F(Fusb302ProtocolTest, TransmitIgnoresPendingGoodCrc) {
  MockReceiveFifoNotEmpty();
  MockReceiveSourceCapabilities(usb_pd::MessageId(0));
  MockReceiveFifoEmpty();

  MockTransmitFifoEmpty();
  MockNoBmcTransmissionDetected();
  MockWritePowerRequestToFifo(usb_pd::MessageId(0));

  EXPECT_OK(protocol_->DrainReceiveFifo());
  EXPECT_TRUE(protocol_->HasUnreadMessage());

  usb_pd::Message request(usb_pd::MessageType::kRequestPower, usb_pd::MessageId(0),
                          usb_pd::PowerRole::kSink, usb_pd::SpecRevision::kRev2,
                          usb_pd::DataRole::kUpstreamFacingPort, MockPowerRequestDataObjects());
  EXPECT_OK(protocol_->Transmit(request));
  EXPECT_EQ(TransmissionState::kPending, protocol_->transmission_state());
}

TEST_F(Fusb302ProtocolTest, DrainReceiveFifoGoodCrcUpdatesTransmissionState) {
  MockTransmitFifoEmpty();
  MockNoBmcTransmissionDetected();
  MockWritePowerRequestToFifo(usb_pd::MessageId(0));

  MockReceiveFifoNotEmpty();
  MockReceiveGoodCrc(usb_pd::MessageId(0));
  MockReceiveFifoEmpty();

  usb_pd::Message request(usb_pd::MessageType::kRequestPower, usb_pd::MessageId(0),
                          usb_pd::PowerRole::kSink, usb_pd::SpecRevision::kRev2,
                          usb_pd::DataRole::kUpstreamFacingPort, MockPowerRequestDataObjects());
  EXPECT_OK(protocol_->Transmit(request));

  EXPECT_OK(protocol_->DrainReceiveFifo());
  EXPECT_FALSE(protocol_->HasUnreadMessage());
  ASSERT_EQ(TransmissionState::kSuccess, protocol_->transmission_state());
  EXPECT_EQ(usb_pd::MessageId(1), protocol_->next_transmitted_message_id());
}

TEST_F(Fusb302ProtocolTest, DrainReceiveFifoSoftResetWithOutOfOrderMessageId) {
  MockReceiveFifoNotEmpty();
  MockReceiveSourceCapabilities(usb_pd::MessageId(0));
  MockReceiveFifoEmpty();

  MockTransmitFifoEmpty();
  MockNoBmcTransmissionDetected();
  MockWriteGoodCrcToFifo(usb_pd::MessageId(0));

  // The incoming MessageID counter will be at 1. The Soft Reset will be
  // accepted even though it does not match this counter.
  MockReceiveFifoNotEmpty();
  MockReceiveSoftReset(usb_pd::MessageId(0));
  MockReceiveFifoEmpty();

  EXPECT_OK(protocol_->DrainReceiveFifo());
  EXPECT_TRUE(protocol_->HasUnreadMessage());
  EXPECT_EQ(usb_pd::MessageType::kSourceCapabilities,
            protocol_->FirstUnreadMessage().header().message_type());

  EXPECT_OK(protocol_->MarkMessageAsRead());
  EXPECT_FALSE(protocol_->HasUnreadMessage());

  EXPECT_OK(protocol_->DrainReceiveFifo());
  EXPECT_TRUE(protocol_->HasUnreadMessage());
  EXPECT_EQ(usb_pd::MessageType::kSoftReset,
            protocol_->FirstUnreadMessage().header().message_type());
}

TEST_F(Fusb302ProtocolTest, DrainReceiveFifoSoftResetWithOutOfOrderMessageIdCreatesPendingGoodCrc) {
  MockReceiveFifoNotEmpty();
  MockReceiveSourceCapabilities(usb_pd::MessageId(0));
  MockReceiveFifoEmpty();

  MockTransmitFifoEmpty();
  MockNoBmcTransmissionDetected();
  MockWriteGoodCrcToFifo(usb_pd::MessageId(0));

  MockReceiveFifoNotEmpty();
  MockReceiveSoftReset(usb_pd::MessageId(0));
  MockReceiveFifoEmpty();

  // Verify that we acknowledge Soft Reset with GoodCRC, and that the GoodCRC
  // uses the SoftReset's MessageID, not the old incoming MessageID counter.
  MockTransmitFifoEmpty();
  MockNoBmcTransmissionDetected();
  MockWriteGoodCrcToFifo(usb_pd::MessageId(0));

  EXPECT_OK(protocol_->DrainReceiveFifo());
  EXPECT_TRUE(protocol_->HasUnreadMessage());
  EXPECT_EQ(usb_pd::MessageType::kSourceCapabilities,
            protocol_->FirstUnreadMessage().header().message_type());

  EXPECT_OK(protocol_->MarkMessageAsRead());
  EXPECT_FALSE(protocol_->HasUnreadMessage());

  EXPECT_OK(protocol_->DrainReceiveFifo());
  EXPECT_TRUE(protocol_->HasUnreadMessage());
  EXPECT_EQ(usb_pd::MessageType::kSoftReset,
            protocol_->FirstUnreadMessage().header().message_type());

  EXPECT_OK(protocol_->MarkMessageAsRead());
}

TEST_F(Fusb302ProtocolTest, DrainReceiveFifoSoftResetDropsPreviousMessages) {
  MockReceiveFifoNotEmpty();
  MockReceiveSourceCapabilities(usb_pd::MessageId(0));
  MockReceiveFifoNotEmpty();
  MockReceiveGetSinkCapabilities(usb_pd::MessageId(1));
  MockReceiveFifoNotEmpty();
  MockReceiveSoftReset(usb_pd::MessageId(0));
  MockReceiveFifoEmpty();

  MockTransmitFifoEmpty();
  MockNoBmcTransmissionDetected();
  MockWriteGoodCrcToFifo(usb_pd::MessageId(0));

  EXPECT_OK(protocol_->DrainReceiveFifo());
  EXPECT_TRUE(protocol_->HasUnreadMessage());
  EXPECT_EQ(usb_pd::MessageType::kSoftReset,
            protocol_->FirstUnreadMessage().header().message_type());

  EXPECT_OK(protocol_->MarkMessageAsRead());
  EXPECT_FALSE(protocol_->HasUnreadMessage());
}

TEST_F(Fusb302ProtocolTest, DrainReceiveFifoQueueingOrder) {
  MockReceiveFifoNotEmpty();
  MockReceiveAccept(usb_pd::MessageId(0));
  MockReceiveFifoNotEmpty();
  MockReceivePowerReady(usb_pd::MessageId(1));
  MockReceiveFifoEmpty();

  EXPECT_OK(protocol_->DrainReceiveFifo());
  ASSERT_TRUE(protocol_->HasUnreadMessage());
  EXPECT_EQ(usb_pd::MessageType::kAccept, protocol_->FirstUnreadMessage().header().message_type());

  // No I2C expected: we got message 1, so we must assume the Port partner got a
  // GoodCRC for message 0.
  EXPECT_OK(protocol_->MarkMessageAsRead());
  ASSERT_TRUE(protocol_->HasUnreadMessage());
  EXPECT_EQ(usb_pd::MessageType::kPowerSupplyReady,
            protocol_->FirstUnreadMessage().header().message_type());
}

TEST_F(Fusb302ProtocolTest, DrainReceiveFifoWithMultipleMessagesSetsPendingCrcToLastMessage) {
  MockReceiveFifoNotEmpty();
  MockReceiveAccept(usb_pd::MessageId(0));
  MockReceiveFifoNotEmpty();
  MockReceivePowerReady(usb_pd::MessageId(1));
  MockReceiveFifoNotEmpty();
  MockReceiveGetSinkCapabilities(usb_pd::MessageId(2));
  MockReceiveFifoEmpty();

  MockTransmitFifoEmpty();
  MockNoBmcTransmissionDetected();
  MockWriteGoodCrcToFifo(usb_pd::MessageId(2));

  EXPECT_OK(protocol_->DrainReceiveFifo());
  ASSERT_TRUE(protocol_->HasUnreadMessage());
  EXPECT_EQ(usb_pd::MessageType::kAccept, protocol_->FirstUnreadMessage().header().message_type());

  // No I2C expected: we got message 1, so we must assume the Port partner got a
  // GoodCRC for message 0.
  EXPECT_OK(protocol_->MarkMessageAsRead());
  ASSERT_TRUE(protocol_->HasUnreadMessage());
  EXPECT_EQ(usb_pd::MessageType::kPowerSupplyReady,
            protocol_->FirstUnreadMessage().header().message_type());

  // No I2C expected: we got message 2, so we must assume the Port partner got a
  // GoodCRC for message 1.
  EXPECT_OK(protocol_->MarkMessageAsRead());
  ASSERT_TRUE(protocol_->HasUnreadMessage());
  EXPECT_EQ(usb_pd::MessageType::kGetSinkCapabilities,
            protocol_->FirstUnreadMessage().header().message_type());

  // GoodCRC for message 2 transmitted here.
  EXPECT_OK(protocol_->MarkMessageAsRead());
  EXPECT_FALSE(protocol_->HasUnreadMessage());
}

TEST_F(Fusb302ProtocolTest, DidTransmitGoodCrcAfterDrainReceiveFifoWithMultipleMessages) {
  MockReceiveFifoNotEmpty();
  MockReceiveAccept(usb_pd::MessageId(0));
  MockReceiveFifoNotEmpty();
  MockReceivePowerReady(usb_pd::MessageId(1));
  MockReceiveFifoNotEmpty();
  MockReceiveGetSinkCapabilities(usb_pd::MessageId(2));
  MockReceiveFifoEmpty();

  EXPECT_OK(protocol_->DrainReceiveFifo());
  protocol_->DidTransmitGoodCrc();

  ASSERT_TRUE(protocol_->HasUnreadMessage());
  EXPECT_EQ(usb_pd::MessageType::kAccept, protocol_->FirstUnreadMessage().header().message_type());

  // No I2C expected: we got message 1, so we must assume the Port partner got a
  // GoodCRC for message 0.
  EXPECT_OK(protocol_->MarkMessageAsRead());
  ASSERT_TRUE(protocol_->HasUnreadMessage());
  EXPECT_EQ(usb_pd::MessageType::kPowerSupplyReady,
            protocol_->FirstUnreadMessage().header().message_type());

  // No I2C expected: we got message 2, so we must assume the Port partner got a
  // GoodCRC for message 1.
  EXPECT_OK(protocol_->MarkMessageAsRead());
  ASSERT_TRUE(protocol_->HasUnreadMessage());
  EXPECT_EQ(usb_pd::MessageType::kGetSinkCapabilities,
            protocol_->FirstUnreadMessage().header().message_type());

  // No I2C expected: we got a "GoodCRC transmitted" signal, and we assume it's
  // related to this message.
  EXPECT_OK(protocol_->MarkMessageAsRead());
  EXPECT_FALSE(protocol_->HasUnreadMessage());
}

TEST_F(Fusb302ProtocolTest, TransmitAfterDrainReceiveFifoWithMultipleMessages) {
  MockReceiveFifoNotEmpty();
  MockReceiveAccept(usb_pd::MessageId(0));
  MockReceiveFifoNotEmpty();
  MockReceivePowerReady(usb_pd::MessageId(1));
  MockReceiveFifoNotEmpty();
  MockReceiveGetSinkCapabilities(usb_pd::MessageId(2));
  MockReceiveFifoEmpty();

  MockTransmitFifoEmpty();
  MockNoBmcTransmissionDetected();
  MockWriteSinkCapabilitiesToFifo(usb_pd::MessageId(0));

  EXPECT_OK(protocol_->DrainReceiveFifo());
  protocol_->DidTransmitGoodCrc();

  ASSERT_TRUE(protocol_->HasUnreadMessage());
  EXPECT_EQ(usb_pd::MessageType::kAccept, protocol_->FirstUnreadMessage().header().message_type());

  EXPECT_OK(protocol_->MarkMessageAsRead());
  ASSERT_TRUE(protocol_->HasUnreadMessage());
  EXPECT_EQ(usb_pd::MessageType::kPowerSupplyReady,
            protocol_->FirstUnreadMessage().header().message_type());

  EXPECT_OK(protocol_->MarkMessageAsRead());
  ASSERT_TRUE(protocol_->HasUnreadMessage());
  EXPECT_EQ(usb_pd::MessageType::kGetSinkCapabilities,
            protocol_->FirstUnreadMessage().header().message_type());

  EXPECT_OK(protocol_->MarkMessageAsRead());
  EXPECT_FALSE(protocol_->HasUnreadMessage());

  usb_pd::Message capabilities(usb_pd::MessageType::kSinkCapabilities, usb_pd::MessageId(0),
                               usb_pd::PowerRole::kSink, usb_pd::SpecRevision::kRev2,
                               usb_pd::DataRole::kUpstreamFacingPort,
                               MockSinkCapabilitiesDataObjects());
  EXPECT_OK(protocol_->Transmit(capabilities));
  EXPECT_EQ(TransmissionState::kPending, protocol_->transmission_state());
}

}  // namespace

}  // namespace fusb302
