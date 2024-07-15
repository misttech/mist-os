// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "ctaphid.h"

#include <endian.h>
#include <fidl/fuchsia.fido.report/cpp/wire.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/fit/defer.h>
#include <lib/hid-parser/parser.h>
#include <lib/hid-parser/report.h>
#include <lib/hid-parser/usages.h>
#include <zircon/status.h>
#include <zircon/syscalls.h>

#include <fbl/alloc_checker.h>
#include <fbl/auto_lock.h>

namespace ctaphid {

namespace fhidbus = fuchsia_hardware_hidbus;
namespace finput = fuchsia_hardware_input;

void CtapHidDriver::CreatePacketHeader(uint8_t packet_sequence, uint32_t channel_id,
                                       fuchsia_fido_report::CtapHidCommand command_id,
                                       uint16_t payload_len, uint8_t* out, size_t out_size) {
  for (size_t i = 0; i < out_size; i++) {
    out[i] = 0;
  }

  // Write the Channel ID.
  out[CHANNEL_ID_OFFSET + 0] = (channel_id >> 24) & 0xFF;
  out[CHANNEL_ID_OFFSET + 1] = (channel_id >> 16) & 0xFF;
  out[CHANNEL_ID_OFFSET + 2] = (channel_id >> 8) & 0xFF;
  out[CHANNEL_ID_OFFSET + 3] = channel_id & 0xFF;

  // Write the rest of the packet header. This differs between initialization and continuation
  // packets.
  if (packet_sequence == INIT_PACKET_SEQ) {
    // Write the Command ID with the initialization packet bit set.
    out[COMMAND_ID_OFFSET] = fidl::ToUnderlying(command_id) | INIT_PACKET_BIT;
    // Write the Payload Length
    out[PAYLOAD_LEN_HI_OFFSET] = (payload_len >> 8) & 0xFF;
    out[PAYLOAD_LEN_LO_OFFSET] = payload_len & 0xFF;

  } else {
    // The packet sequence value, starting at 0.
    out[PACKET_SEQ_OFFSET] = packet_sequence;
  }
}

zx_status_t CtapHidDriver::Start() {
  fidl::WireResult result = input_device_->GetReportDesc();
  if (!result.ok()) {
    zxlogf(ERROR, "GetReportDesc failed %d", result.status());
    return result.status();
  }

  hid::DeviceDescriptor* dev_desc = nullptr;
  hid::ParseResult parse_res =
      hid::ParseReportDescriptor(result->desc.data(), result->desc.count(), &dev_desc);
  if (parse_res != hid::ParseResult::kParseOk) {
    zxlogf(ERROR, "hid-parser: parsing report descriptor failed with error %d", int(parse_res));
    return ZX_ERR_INTERNAL;
  }
  auto free_desc = fit::defer([dev_desc]() { hid::FreeDeviceDescriptor(dev_desc); });

  if (dev_desc->rep_count == 0) {
    zxlogf(ERROR, "No report descriptors found ");
    return ZX_ERR_INTERNAL;
  }

  const hid::ReportDescriptor* desc = &dev_desc->report[0];
  output_packet_size_ = desc->output_byte_sz;
  output_packet_id_ = desc->output_fields->report_id;

  // Payload size calculation taken from the CTAP specification v2.1-ps-20210615 section 11.2.4.
  max_output_data_size_ = output_packet_size_ - INITIALIZATION_PAYLOAD_DATA_OFFSET +
                          MAX_PACKET_SEQ * (output_packet_size_ - CONTINUATION_PAYLOAD_DATA_OFFSET);

  // Register to listen for HID reports.
  auto [client, server] = fidl::Endpoints<finput::DeviceReportsReader>::Create();
  auto status = input_device_->GetDeviceReportsReader(std::move(server));
  if (!status.ok()) {
    zxlogf(ERROR, "Failed to get device reports reader: %s", status.FormatDescription().c_str());
    return status.status();
  }
  dev_reader_.Bind(std::move(client), fdf::Dispatcher::GetCurrent()->async_dispatcher());
  dev_reader_->ReadReports().Then(fit::bind_member<&CtapHidDriver::HandleReports>(this));

  return ZX_OK;
}

zx_status_t CtapHidDriver::Bind() {
  zx_status_t status = Start();
  if (status != ZX_OK) {
    return status;
  }
  status = DdkAdd(ddk::DeviceAddArgs("SecurityKey"));
  if (status != ZX_OK) {
    return status;
  }
  return ZX_OK;
}

void CtapHidDriver::DdkUnbind(ddk::UnbindTxn txn) { txn.Reply(); }

void CtapHidDriver::DdkRelease() { delete this; }

void CtapHidDriver::SendMessage(SendMessageRequestView request,
                                SendMessageCompleter::Sync& completer) {
  fbl::AutoLock lock(&lock_);
  // Check the device is capable of receiving this message's payload size.
  if (request->payload_len() > max_output_data_size_) {
    completer.ReplyError(ZX_ERR_OUT_OF_RANGE);
    return;
  }

  // Ensure there is only one outgoing request at a time to maintain transaction atomicity.
  if (pending_response_) {
    completer.ReplyError(ZX_ERR_UNAVAILABLE);
    return;
  }

  // Send the message to the device.
  channel_id_t channel_id = request->channel_id();

  // Divide up the request's data into a series of packets, starting with an initialization packet.
  auto data_it = request->data().begin();
  if (request->data().empty()) {
    uint8_t curr_hid_report[fhidbus::wire::kMaxReportLen];
    CreatePacketHeader(INIT_PACKET_SEQ, channel_id, request->command_id(), request->payload_len(),
                       curr_hid_report, fhidbus::wire::kMaxReportLen);

    auto result = input_device_->SetReport(
        fhidbus::ReportType::kOutput, output_packet_id_,
        fidl::VectorView<uint8_t>::FromExternal(curr_hid_report, output_packet_size_));
    if (!result.ok()) {
      completer.ReplyError(result.status());
      return;
    }
  }

  for (uint8_t packet_sequence = INIT_PACKET_SEQ;
       (packet_sequence < MAX_PACKET_SEQ || packet_sequence == INIT_PACKET_SEQ) &&
       data_it != request->data().end();
       packet_sequence++) {
    uint8_t curr_hid_report[fhidbus::wire::kMaxReportLen];
    CreatePacketHeader(packet_sequence, channel_id, request->command_id(), request->payload_len(),
                       curr_hid_report, fhidbus::wire::kMaxReportLen);

    // Write the payload.
    size_t byte_n = packet_sequence == INIT_PACKET_SEQ ? INITIALIZATION_PAYLOAD_DATA_OFFSET
                                                       : CONTINUATION_PAYLOAD_DATA_OFFSET;
    for (; byte_n < output_packet_size_ && data_it != request->data().end(); byte_n++) {
      curr_hid_report[byte_n] = *data_it;
      data_it++;
    }

    auto result = input_device_->SetReport(
        fhidbus::ReportType::kOutput, output_packet_id_,
        fidl::VectorView<uint8_t>::FromExternal(curr_hid_report, output_packet_size_));
    if (!result.ok()) {
      completer.ReplyError(result.status());
      return;
    }
  }

  // Set the pending response. The pending response will be reset once the device has sent a
  // response and it has been retrieved via GetMessage().
  // TODO(https://fxbug.dev/42054989): have this clear after some time or when the list gets too
  // large.
  pending_response_ = pending_response{
      .channel = channel_id,
      .next_packet_seq_expected = INIT_PACKET_SEQ,
  };

  completer.ReplySuccess();
}

void CtapHidDriver::GetMessage(GetMessageRequestView request,
                               GetMessageCompleter::Sync& completer) {
  fbl::AutoLock lock(&lock_);

  if (pending_response_->channel == request->channel_id) {
    if (pending_response_->waiting_read) {
      completer.ReplyError(ZX_ERR_ALREADY_BOUND);
      return;
    }

    pending_response_->waiting_read = completer.ToAsync();
    ReplyToWaitingGetMessage();

    return;
  }

  // If no matching response or pending request was found, either the response had timed out or no
  // matching request had been made.
  completer.ReplyError(ZX_ERR_NOT_FOUND);
}

void CtapHidDriver::ReplyToWaitingGetMessage() {
  if (!pending_response_->waiting_read) {
    // Return if there is no waiting read to reply to.
    return;
  }

  if (!pending_response_->last_packet_received_time.has_value()) {
    // We are still waiting on a response.
    return;
  }

  auto response_builder_ = fuchsia_fido_report::wire::Message::Builder(response_allocator_);
  response_builder_.channel_id(pending_response_->channel);
  response_builder_.command_id(
      fuchsia_fido_report::CtapHidCommand(pending_response_->command.value()));
  response_builder_.payload_len(pending_response_->payload_len.value());
  response_builder_.data(fidl::VectorView<uint8_t>::FromExternal(pending_response_->data));
  pending_response_->waiting_read->ReplySuccess(response_builder_.Build());
  fidl::Status result = pending_response_->waiting_read->result_of_reply();
  if (!result.ok()) {
    zxlogf(ERROR, "GetMessage: Failed to get message: %s\n", result.FormatDescription().c_str());
  }
  pending_response_->waiting_read.reset();
  response_allocator_.Reset();

  // Remove the pending response if this is not a KEEPALIVE message, as KEEPALIVE
  // messages are not considered an actual response to any command sent to the keys.
  if (pending_response_->command !=
      fidl::ToUnderlying(fuchsia_fido_report::CtapHidCommand::kKeepalive)) {
    pending_response_.reset();
  }
}

void CtapHidDriver::HandleReports(
    fidl::WireUnownedResult<fuchsia_hardware_input::DeviceReportsReader::ReadReports>& result) {
  if (!result.ok()) {
    return;
  }
  if (result->is_error()) {
    return;
  }

  dev_reader_->ReadReports().Then(fit::bind_member<&CtapHidDriver::HandleReports>(this));

  for (const auto& report : result->value()->reports) {
    fbl::AutoLock _(&lock_);
    HandleReport(cpp20::span<uint8_t>(report.buf().data(), report.buf().count()),
                 zx::time(report.timestamp()));
  }
}

void CtapHidDriver::HandleReport(cpp20::span<uint8_t> report, zx::time report_time) {
  channel_id_t current_channel =
      (report[0] << 24) | (report[1] << 16) | (report[2] << 8) | report[3];
  uint8_t data_size = static_cast<uint8_t>(report.size());

  if (pending_response_->channel != current_channel) {
    // This means that we have received an unexpected as there is no pending request on this channel
    // as far as the driver is aware. Ignore this packet.
    return;
  }

  if (report[COMMAND_ID_OFFSET] & INIT_PACKET_BIT) {
    if (pending_response_->next_packet_seq_expected != INIT_PACKET_SEQ &&
        pending_response_->command !=
            fidl::ToUnderlying(fuchsia_fido_report::CtapHidCommand::kKeepalive)) {
      // Unexpected sequence. We must be out of sync.
      // Write an invalid sequence error response to the corresponding pending response.
      pending_response_->command = fidl::ToUnderlying(fuchsia_fido_report::CtapHidCommand::kError);
      pending_response_->bytes_received = 1;
      pending_response_->payload_len = 1;
      pending_response_->data = std::vector<uint8_t>(CtaphidErr::InvalidSeq);
      pending_response_->last_packet_received_time.emplace(report_time);

      return;
    }

    command_id_t command_id = report[COMMAND_ID_OFFSET] & ~INIT_PACKET_BIT;
    data_size -= INITIALIZATION_PAYLOAD_DATA_OFFSET;

    pending_response_->bytes_received = data_size;
    pending_response_->payload_len = report[PAYLOAD_LEN_HI_OFFSET] << 8;
    pending_response_->payload_len =
        pending_response_->payload_len.value() | report[PAYLOAD_LEN_LO_OFFSET];

    pending_response_->data =
        std::vector<uint8_t>(&report[INITIALIZATION_PAYLOAD_DATA_OFFSET],
                             &report[INITIALIZATION_PAYLOAD_DATA_OFFSET] + data_size);
    pending_response_->command = command_id;
    pending_response_->next_packet_seq_expected = MIN_PACKET_SEQ;

  } else {
    auto current_packet_sequence = report[PACKET_SEQ_OFFSET];
    data_size -= CONTINUATION_PAYLOAD_DATA_OFFSET;
    if (current_packet_sequence != pending_response_->next_packet_seq_expected) {
      // Unexpected sequence. We must be out of sync.
      // Write an invalid sequence error response to the corresponding pending response.
      pending_response_->command = fidl::ToUnderlying(fuchsia_fido_report::CtapHidCommand::kError);
      pending_response_->bytes_received = 1;
      pending_response_->payload_len = 1;
      pending_response_->data = std::vector<uint8_t>(CtaphidErr::InvalidSeq);
      pending_response_->last_packet_received_time.emplace(report_time);

      return;
    }

    pending_response_->data.insert(pending_response_->data.end(),
                                   &report[CONTINUATION_PAYLOAD_DATA_OFFSET],
                                   &report[CONTINUATION_PAYLOAD_DATA_OFFSET] + data_size);
    pending_response_->bytes_received += data_size;
    pending_response_->next_packet_seq_expected += 1;
  }

  if (pending_response_->bytes_received >= pending_response_->payload_len) {
    // We have finished receiving packets for this response.
    pending_response_->last_packet_received_time.emplace(report_time);
    ReplyToWaitingGetMessage();
  }
}

zx_status_t ctaphid_bind(void* ctx, zx_device_t* parent) {
  zx::result<fidl::ClientEnd<finput::Controller>> controller =
      ddk::Device<void>::DdkConnectFidlProtocol<finput::Service::Controller>(parent);
  if (!controller.is_ok()) {
    return ZX_ERR_INTERNAL;
  }

  auto [client, server] = fidl::Endpoints<finput::Device>::Create();
  auto result = fidl::WireCall(controller.value())->OpenSession(std::move(server));
  if (!result.ok()) {
    return result.status();
  }

  fbl::AllocChecker ac;
  auto dev = fbl::make_unique_checked<CtapHidDriver>(&ac, parent, std::move(client));
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }
  auto status = dev->Bind();
  if (status == ZX_OK) {
    // devmgr is now in charge of the memory for dev
    [[maybe_unused]] auto ptr = dev.release();
  }
  return status;
}

static zx_driver_ops_t ctaphid_driver_ops = []() -> zx_driver_ops_t {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = ctaphid_bind;
  return ops;
}();

}  // namespace ctaphid

ZIRCON_DRIVER(ctaphid, ctaphid::ctaphid_driver_ops, "zircon", "0.1");
