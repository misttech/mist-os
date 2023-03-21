// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/power/drivers/fusb302/fusb302-fifos.h"

#include <fidl/fuchsia.hardware.i2c/cpp/wire.h>
#include <lib/ddk/debug.h>
#include <lib/fidl/cpp/wire/arena.h>
#include <lib/stdcompat/span.h>
#include <lib/zx/result.h>
#include <lib/zx/time.h>
#include <zircon/assert.h>
#include <zircon/status.h>

#include <cstdint>
#include <optional>
#include <type_traits>
#include <utility>

#include <fbl/string_buffer.h>

#include "src/devices/power/drivers/fusb302/registers.h"
#include "src/devices/power/drivers/fusb302/usb-pd-defs.h"
#include "src/devices/power/drivers/fusb302/usb-pd-message-type.h"
#include "src/devices/power/drivers/fusb302/usb-pd-message.h"

namespace fusb302 {

Fusb302Fifos::Fusb302Fifos(fidl::ClientEnd<fuchsia_hardware_i2c::Device>& i2c_channel)
    : i2c_(i2c_channel) {}

static_assert(std::is_trivially_destructible_v<Fusb302Fifos>,
              "Move non-trivial destructors outside the header");

zx::result<std::optional<usb_pd::Message>> Fusb302Fifos::ReadReceivedMessage() {
  auto status1 = Status1Reg::ReadFrom(i2c_);
  if (status1.rx_empty()) {
    return zx::ok(std::nullopt);
  }

  // Every message has an SOP token and a header.
  uint8_t message_start_bytes[3];
  zx::result<> result = FifoI2cRead(message_start_bytes);
  if (result.is_error()) {
    zxlogf(ERROR, "Failed to read packet start from Fifos register: %s", result.status_string());
    return result.take_error();
  }
  zxlogf(TRACE, "Received SOP token and header bytes - 0x%02x 0x%02x 0x%02x",
         message_start_bytes[0], message_start_bytes[1], message_start_bytes[2]);

  const ReceiveTokenType token_type = FifosReg::AsReceiveTokenType(message_start_bytes[0]);
  if (token_type == ReceiveTokenType::kUndocumented) {
    zxlogf(ERROR, "Receive FIFO produced undocumented SOP token: 0x%02x", message_start_bytes[0]);

    // TODO(costan): Flush the RX queue?
    return zx::error(ZX_ERR_INTERNAL);
  }

  usb_pd::Header header =
      usb_pd::Header::CreateFromBytes(message_start_bytes[1], message_start_bytes[2]);
  zxlogf(TRACE, "Received header - %s, %d data objects, message ID: %" PRIu8 ", extended: %s ",
         usb_pd::MessageTypeToString(header.message_type()), header.data_object_count(),
         static_cast<uint8_t>(header.message_id()), header.is_extended() ? "yes" : "no");

  // Read the data objects and CRC.
  static constexpr int kCrcSize = 4;
  ZX_DEBUG_ASSERT(header.data_object_count() <= usb_pd::Header::kMaxDataObjectCount);

  // The CRC size matches the data object size.
  std::array<uint32_t, usb_pd::Message::kMaxPayloadBytes + 1> data_objects;
  int16_t payload_size = header.payload_bytes();
  int16_t payload_plus_crc_size = static_cast<int16_t>(payload_size + kCrcSize);
  result = FifoI2cRead(
      cpp20::span(reinterpret_cast<uint8_t*>(data_objects.data()), payload_plus_crc_size));
  if (result.is_error()) {
    zxlogf(ERROR, "Failed to read packet data from Fifos register: %s", result.status_string());
    return result.take_error();
  }

  if (zxlog_level_enabled(TRACE)) {
    static constexpr int kMaxStringSize = usb_pd::Header::kMaxDataObjectCount * 11;
    fbl::StringBuffer<kMaxStringSize> data_objects_string;
    for (size_t i = 0; i < header.data_object_count(); ++i) {
      data_objects_string.AppendPrintf("0x%08x ", data_objects[i]);
    }
    zxlogf(TRACE, "Received %d data objects - %s", header.data_object_count(),
           data_objects_string.c_str());
  }

  // The CRC was already checked by the PD layer in the FUSB302. We ignore it
  // after logging (in case it aids debugging).
  zxlogf(TRACE, "Received CRC - 0x%08x", data_objects[header.data_object_count()]);

  return zx::ok(
      usb_pd::Message(header, cpp20::span(data_objects.data(), header.data_object_count())));
}

zx::result<> Fusb302Fifos::FifoI2cRead(cpp20::span<uint8_t> read_output) {
  uint8_t i2c_address = static_cast<uint8_t>(FifosReg::Get().addr());

  fidl::Arena arena;
  fidl::VectorView<fuchsia_hardware_i2c::wire::Transaction> transactions(arena, 2);
  transactions[0] = fuchsia_hardware_i2c::wire::Transaction::Builder(arena)
                        .data_transfer(fuchsia_hardware_i2c::wire::DataTransfer::WithWriteData(
                            arena, fidl::VectorView<uint8_t>::FromExternal(&i2c_address, 1)))
                        .Build();
  transactions[1] =
      fuchsia_hardware_i2c::wire::Transaction::Builder(arena)
          .data_transfer(fuchsia_hardware_i2c::wire::DataTransfer::WithReadSize(read_output.size()))
          .Build();

  auto response = fidl::WireCall(i2c_)->Transfer(transactions);
  if (!response.ok()) {
    return zx::error_result(response.status());
  }
  if (response.value().is_error()) {
    return zx::error_result(response.value().error_value());
  }
  if (response.value().value()->read_data.count() != 1) {
    zxlogf(ERROR, "I2C Read request succeeded, but returned an incorrect item count");
    return zx::error_result(ZX_ERR_BAD_STATE);
  }
  if (response.value().value()->read_data[0].count() != read_output.size()) {
    zxlogf(ERROR, "I2C Read request succeeded, but returned an incorrect byte count");
    return zx::error_result(ZX_ERR_BAD_STATE);
  }

  std::copy(response->value()->read_data[0].begin(), response->value()->read_data[0].end(),
            read_output.begin());
  return zx::ok();
}

namespace {

// Counts the bytes needed by FUSB302's transmitter FIFO to send a message.
size_t SerializedMessageSize(const usb_pd::Message& message) {
  // Overhead: 4 tokens for ordered set, packet data token, CRC token, end of packet token.
  return message.header().message_bytes() + 7;
}

// Writes the bytes needed by FUSB302's transmitter FIFO to send a message.
void SerializeMessage(const usb_pd::Message& message, cpp20::span<uint8_t> output_bytes) {
  ZX_DEBUG_ASSERT(output_bytes.size() == SerializedMessageSize(message));

  // SOP (Start of Packet) ordered set.
  //
  // usbpd3.1 5.6.1.2.1 "Start of Packet Sequence (SOP)"
  output_bytes[0] = TransmitToken::kSync1;
  output_bytes[1] = TransmitToken::kSync1;
  output_bytes[2] = TransmitToken::kSync1;
  output_bytes[3] = TransmitToken::kSync2;

  // Data marker and packet header.
  output_bytes[4] = TransmitToken::PacketData(message.header().message_bytes());
  std::tie(output_bytes[5], output_bytes[6]) = message.header().bytes();

  // Data objects.

  // The cast will not overflow (causing UB) because the maximum message size is
  // less than 100 bytes.
  const int32_t message_payload_bytes = static_cast<int32_t>(message.data_objects().size_bytes());
  std::memcpy(&output_bytes[7], message.data_objects().data(), message_payload_bytes);

  // Packet trailer - CRC, EOP, transmitter power management.
  output_bytes[7 + message_payload_bytes] = TransmitToken::kInsertCrc;
  output_bytes[8 + message_payload_bytes] = TransmitToken::kEop;
}

}  // namespace

zx::result<> Fusb302Fifos::TransmitMessage(const usb_pd::Message& main_message) {
  static constexpr size_t kI2cHeaderSize = 1;
  static constexpr size_t kMaxMessageSerializedSize = 7 + usb_pd::Message::kMaxMessageBytes;
  static constexpr size_t kTransmitterOnOffSize = 2;
  uint8_t i2c_write_bytes[kI2cHeaderSize + kMaxMessageSerializedSize + kTransmitterOnOffSize];

  i2c_write_bytes[0] = FifosReg::Get().addr();
  size_t i2c_write_offset = kI2cHeaderSize;

  zxlogf(TRACE, "Transmitting header - %s, %d data objects, message ID: %" PRIu8 ", extended: %s ",
         usb_pd::MessageTypeToString(main_message.header().message_type()),
         main_message.header().data_object_count(),
         static_cast<uint8_t>(main_message.header().message_id()),
         main_message.header().is_extended() ? "yes" : "no");

  if (zxlog_level_enabled(TRACE)) {
    static constexpr int kMaxStringSize = usb_pd::Header::kMaxDataObjectCount * 11;
    fbl::StringBuffer<kMaxStringSize> data_objects_string;
    for (size_t i = 0; i < main_message.header().data_object_count(); ++i) {
      data_objects_string.AppendPrintf("0x%08x ", main_message.data_objects()[i]);
    }
    zxlogf(TRACE, "Transmitting %d data objects - %s", main_message.header().data_object_count(),
           data_objects_string.c_str());
  }

  const size_t message_size = SerializedMessageSize(main_message);
  SerializeMessage(main_message,
                   cpp20::span<uint8_t>(&i2c_write_bytes[i2c_write_offset], message_size));
  i2c_write_offset += message_size;

  i2c_write_bytes[i2c_write_offset] = TransmitToken::kTxOff;
  i2c_write_bytes[i2c_write_offset + 1] = TransmitToken::kTxOn;
  i2c_write_offset += 2;

  if (zxlog_level_enabled(TRACE)) {
    static constexpr int kMaxStringSize = sizeof(i2c_write_bytes) * 3;
    fbl::StringBuffer<kMaxStringSize> fifo_bytes_string;
    for (size_t i = 0; i < i2c_write_offset; ++i) {
      fifo_bytes_string.AppendPrintf("%02x ", i2c_write_bytes[i]);
    }
    zxlogf(TRACE, "I2C write bytes - %s", fifo_bytes_string.c_str());
  }

  zx::result<> result = FifoI2cWrite(cpp20::span(i2c_write_bytes, i2c_write_offset));
  if (result.is_error()) {
    auto control0 = Control0Reg::ReadFrom(i2c_);
    zx_status_t status = control0.set_tx_flush(true).WriteTo(i2c_);
    if (status != ZX_OK) {
      zxlogf(ERROR, "Failed to flush Transmitter FIFO after I/O error: %s",
             zx_status_get_string(status));
    }
  }
  return result;
}

zx::result<> Fusb302Fifos::FifoI2cWrite(cpp20::span<uint8_t> i2c_write_bytes) {
  ZX_DEBUG_ASSERT(!i2c_write_bytes.empty());
  ZX_DEBUG_ASSERT(i2c_write_bytes[0] == FifosReg::Get().addr());

  // Experiments show that we may get I2C timeouts if we try to write to the
  // transmitter FIFO when the BMC PHY is already transmitting a message.
  //
  // FUSB302 drivers that use automated GoodCRC replies don't need this
  // check, because they never transmit two messages back-to-back.
  while (!Status1Reg::ReadFrom(i2c_).tx_empty()) {
    zx::nanosleep(zx::deadline_after(zx::usec(10)));
  }

  // Make sure we're not transmitting at the same time as the port partner.
  // This check needs to happen after any zxlogf().
  //
  // FUSB302 drivers that use automated GoodCRC replies don't need this check,
  // because they only use the transmitter FIFO when it's clearly their turn to
  // transfer. GoodCRC replies can overlap with retransmission attempts.
  while (Status0Reg::ReadFrom(i2c_).activity()) {
    zx::nanosleep(zx::deadline_after(zx::usec(10)));
  }

  auto write_data =
      fidl::VectorView<uint8_t>::FromExternal(i2c_write_bytes.data(), i2c_write_bytes.size());

  fidl::Arena arena;
  fidl::VectorView<fuchsia_hardware_i2c::wire::Transaction> transactions(arena, 1);
  transactions[0] =
      fuchsia_hardware_i2c::wire::Transaction::Builder(arena)
          .data_transfer(fuchsia_hardware_i2c::wire::DataTransfer::WithWriteData(arena, write_data))
          .Build();

  auto response = fidl::WireCall(i2c_)->Transfer(transactions);
  if (!response.ok()) {
    return zx::error_result(response.status());
  }
  if (response.value().is_error()) {
    return zx::error_result(response.value().error_value());
  }
  return zx::ok();
}

}  // namespace fusb302
