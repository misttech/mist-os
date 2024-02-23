// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "vendor_hci.h"

#include <lib/zx/clock.h>
#include <lib/zx/object.h>
#include <lib/zx/time.h>
#include <zircon/status.h>
#include <zircon/syscalls.h>

#include <algorithm>

#include <fbl/algorithm.h>

#include "logging.h"

namespace btintel {

using ::bt::hci::CommandPacket;

namespace {

constexpr size_t kMaxSecureSendArgLen = 252;
constexpr auto kInitTimeoutMs = zx::sec(10);

}  // namespace

VendorHci::VendorHci(zx::channel* ctrl) : ctrl_(ctrl), acl_(nullptr), manufacturer_(false) {}

// Fetch unsigned integer values from 'p' with 'fetch_len' bytes at maximum (little-endian).
uint32_t fetch_tlv_value(const uint8_t* p, size_t fetch_len) {
  size_t len = p[1];
  uint32_t val = 0;  // store the return value.

  ZX_DEBUG_ASSERT(len <= sizeof(val));
  ZX_DEBUG_ASSERT(len >= fetch_len);

  // Only load the actual number of bytes .
  fetch_len = fetch_len > len ? len : fetch_len;

  p += 2;  // Skip 'type' and 'length'. Now points to 'value'.
  for (size_t i = fetch_len; i > 0; i--) {
    val = (val << 8) + p[i - 1];
  }

  return val;
}

ReadVersionReturnParamsTlv parse_tlv_version_return_params(const uint8_t* p, size_t len) {
  ReadVersionReturnParamsTlv params = {};

  // Ensure the given byte stream contains the status code, type, and length fields.
  if (len <= 2)
    return ReadVersionReturnParamsTlv{
        .status = pw::bluetooth::emboss::StatusCode::PARAMETER_OUT_OF_MANDATORY_RANGE};

  // The first byte is the status code. Extract and strip it before we traverse the TLVs.
  params.status = static_cast<pw::bluetooth::emboss::StatusCode>(*(p++));
  len--;

  for (size_t idx = 0; idx < len;) {
    // ensure at least the Tag and the Length fields.
    size_t remain_len = len - idx;
    if (remain_len < 2)
      return ReadVersionReturnParamsTlv{
          .status = pw::bluetooth::emboss::StatusCode::PARAMETER_OUT_OF_MANDATORY_RANGE};

    // After excluding the Type and the Length field, ensure there is still room for the Value
    // field.
    size_t len_of_value = p[idx + 1];
    if ((remain_len - 2) < len_of_value)
      return ReadVersionReturnParamsTlv{
          .status = pw::bluetooth::emboss::StatusCode::PARAMETER_OUT_OF_MANDATORY_RANGE};

    switch (p[idx]) {
      uint32_t v;

      case 0x00:  // End of the TLV records.
        break;

      case 0x10:  // CNVi hardware version
        v = fetch_tlv_value(&p[idx], 4);
        params.CNVi = (((v >> 0) & 0xf) << 12) | (((v >> 4) & 0xf) << 0) | (((v >> 8) & 0xf) << 4) |
                      (((v >> 24) & 0xf) << 8);
        break;

      case 0x11:  // CNVR hardware version
        v = fetch_tlv_value(&p[idx], 4);
        params.CNVR = (((v >> 0) & 0xf) << 12) | (((v >> 4) & 0xf) << 0) | (((v >> 8) & 0xf) << 4) |
                      (((v >> 24) & 0xf) << 8);
        break;

      case 0x12:  // hardware info
        v = fetch_tlv_value(&p[idx], 4);
        params.hw_platform = (v >> 8) & 0xff;  // 0x37 for now.
        params.hw_variant = (v >> 16) & 0x3f;  // 0x17 -- Typhoon Peak
                                               // 0x1c -- Gale Peak
        break;

      case 0x16:  // Device revision
        params.device_revision = fetch_tlv_value(&p[idx], 2);
        break;

      case 0x1c:  // Current mode of operation
        params.current_mode_of_operation = fetch_tlv_value(&p[idx], 1);
        break;

      case 0x1d:  // Timestamp
        v = fetch_tlv_value(&p[idx], 2);
        params.timestamp_calendar_week = v >> 0;
        params.timestamp_year = v >> 8;
        break;

      case 0x1e:  // Build type
        params.build_type = fetch_tlv_value(&p[idx], 1);
        break;

      case 0x1f:  // Build number (it can be either 1 or 4 bytes).
        params.build_number = fetch_tlv_value(&p[idx], 4);
        break;

      case 0x28:  // Secure boot
        params.secure_boot = fetch_tlv_value(&p[idx], 1);
        break;

      case 0x2a:  // OTP lock
        params.otp_lock = fetch_tlv_value(&p[idx], 1);
        break;

      case 0x2b:  // API lock
        params.api_lock = fetch_tlv_value(&p[idx], 1);
        break;

      case 0x2c:  // debug lock
        params.debug_lock = fetch_tlv_value(&p[idx], 1);
        break;

      case 0x2d:  // Firmware build
        v = fetch_tlv_value(&p[idx], 3);
        params.firmware_build_number = v >> 0;
        params.firmware_build_calendar_week = v >> 8;
        params.firmware_build_year = v >> 16;
        break;

      case 0x2f:  // Secure boot engine type
        params.secure_boot_engine_type = fetch_tlv_value(&p[idx], 1);
        infof("Secure boot engine type: 0x%02x", params.secure_boot_engine_type);
        break;

      case 0x30:                             // Bluetooth device address
        ZX_DEBUG_ASSERT(len_of_value == 6);  // expect the address length is 6.
        memcpy(params.bluetooth_address, &p[idx + 2], sizeof(params.bluetooth_address));
        break;

      default:
        // unknown tag. skip it.
        warnf("Unknown firmware version TLV tag=0x%02x", p[idx]);
        break;
    }
    idx += 2 + len_of_value;  // Skip the 'length' and 'value'.
  }

  return params;
}

ReadVersionReturnParams VendorHci::SendReadVersion() const {
  auto packet = CommandPacket::New(kReadVersion);
  SendCommand(packet->view());
  auto evt_packet = WaitForEventPacket();
  if (evt_packet) {
    auto params = evt_packet->return_params<ReadVersionReturnParams>();
    if (params)
      return *params;
  }
  errorf("VendorHci: ReadVersion: Error reading response!");
  return ReadVersionReturnParams{.status = pw::bluetooth::emboss::StatusCode::UNSPECIFIED_ERROR};
}

ReadVersionReturnParamsTlv VendorHci::SendReadVersionTlv() const {
  auto packet = CommandPacket::New(kReadVersion, sizeof(VersionCommandParams));
  auto params = packet->mutable_payload<VersionCommandParams>();
  params->para0 = kVersionSupportTlv;  // Only meaningful for AX210 and later.
  SendCommand(packet->view());
  auto evt_packet = WaitForEventPacket();
  if (evt_packet) {
    size_t packet_size =
        evt_packet->view().payload_size() - sizeof(bt::hci_spec::CommandCompleteEventParams);
    const uint8_t* p = evt_packet->return_params<uint8_t>();
    if (p)
      return parse_tlv_version_return_params(p, packet_size);
  }
  errorf("VendorHci: ReadVersionTlv: Error reading response!");
  return ReadVersionReturnParamsTlv{.status = pw::bluetooth::emboss::StatusCode::UNSPECIFIED_ERROR};
}

ReadBootParamsReturnParams VendorHci::SendReadBootParams() const {
  auto packet = CommandPacket::New(kReadBootParams);
  SendCommand(packet->view());
  auto evt_packet = WaitForEventPacket();
  if (evt_packet) {
    auto params = evt_packet->return_params<ReadBootParamsReturnParams>();
    if (params)
      return *params;
  }
  errorf("VendorHci: ReadBootParams: Error reading response!");
  return ReadBootParamsReturnParams{.status = pw::bluetooth::emboss::StatusCode::UNSPECIFIED_ERROR};
}

pw::bluetooth::emboss::StatusCode VendorHci::SendHciReset() const {
  auto packet = CommandPacket::New(bt::hci_spec::kReset);
  SendCommand(packet->view());

  // TODO(armansito): Consider collecting a metric for initialization time
  // (successful and failing) to provide us with a better sense of how long
  // these timeouts should be.
  auto evt_packet = WaitForEventPacket(kInitTimeoutMs, bt::hci_spec::kCommandCompleteEventCode);
  if (!evt_packet) {
    errorf("VendorHci: failed while waiting for HCI_Reset response");
    return pw::bluetooth::emboss::StatusCode::UNSPECIFIED_ERROR;
  }

  const auto* params = evt_packet->return_params<bt::hci_spec::SimpleReturnParams>();
  if (!params) {
    errorf("VendorHci: HCI_Reset: received malformed response");
    return pw::bluetooth::emboss::StatusCode::UNSPECIFIED_ERROR;
  }

  return params->status;
}

void VendorHci::SendVendorReset(uint32_t boot_address) const {
  auto packet = CommandPacket::New(kReset, sizeof(ResetCommandParams));
  auto params = packet->mutable_payload<ResetCommandParams>();
  params->reset_type = 0x00;
  params->patch_enable = 0x01;
  params->ddc_reload = 0x00;
  params->boot_option = 0x01;
  params->boot_address = htobe32(boot_address);

  SendCommand(packet->view());

  // Sleep for 2 seconds to let the controller process the reset.
  zx_nanosleep(zx_deadline_after(ZX_SEC(2)));
}

bool VendorHci::SendSecureSend(uint8_t type, const bt::BufferView& bytes) const {
  size_t left = bytes.size();
  while (left > 0) {
    size_t frag_len = std::min(left, kMaxSecureSendArgLen);
    auto cmd = CommandPacket::New(kSecureSend, frag_len + 1);
    auto data = cmd->mutable_view()->mutable_payload_data();
    data[0] = type;
    data.Write(bytes.view(bytes.size() - left, frag_len), 1);

    SendCommand(cmd->view());
    std::unique_ptr<bt::hci::EventPacket> event = WaitForEventPacket();
    if (!event) {
      errorf("VendorHci: SecureSend: Error reading response!");
      return false;
    }
    if (event->event_code() == bt::hci_spec::kCommandCompleteEventCode) {
      const auto& event_params = event->view().payload<bt::hci_spec::CommandCompleteEventParams>();
      if (le16toh(event_params.command_opcode) != kSecureSend) {
        errorf("VendorHci: Received command complete for something else!");
      } else if (event_params.return_parameters[0] != 0x00) {
        errorf("VendorHci: Received 0x%x instead of zero in command complete!",
               event_params.return_parameters[0]);
        return false;
      }
    } else if (event->event_code() == bt::hci_spec::kVendorDebugEventCode) {
      const auto& params = event->view().template payload<SecureSendEventParams>();
      infof("VendorHci: SecureSend result 0x%x, opcode: 0x%x, status: 0x%x", params.result,
            params.opcode, params.status);
      if (params.result) {
        errorf("VendorHci: Result of %d indicates some error!", params.result);
        return false;
      }
    }
    left -= frag_len;
  }
  return true;
}

bool VendorHci::SendAndExpect(const bt::PacketView<bt::hci_spec::CommandHeader>& command,
                              std::deque<bt::BufferView> events) const {
  SendCommand(command);

  while (events.size() > 0) {
    auto evt_packet = WaitForEventPacket();
    if (!evt_packet) {
      return false;
    }
    auto expected = events.front();
    if ((evt_packet->view().size() != expected.size()) ||
        (memcmp(evt_packet->view().data().data(), expected.data(), expected.size()) != 0)) {
      errorf("VendorHci: SendAndExpect: unexpected event received");
      return false;
    }
    events.pop_front();
  }

  return true;
}

void VendorHci::EnterManufacturerMode() {
  if (manufacturer_)
    return;

  auto packet = CommandPacket::New(kMfgModeChange, sizeof(MfgModeChangeCommandParams));
  auto params = packet->mutable_payload<MfgModeChangeCommandParams>();
  params->enable = pw::bluetooth::emboss::GenericEnableParam::ENABLE;
  params->disable_mode = MfgDisableMode::kNoPatches;

  SendCommand(packet->view());
  auto evt_packet = WaitForEventPacket();
  if (!evt_packet || evt_packet->event_code() != bt::hci_spec::kCommandCompleteEventCode) {
    errorf("VendorHci: EnterManufacturerMode failed");
    return;
  }

  manufacturer_ = true;
}

bool VendorHci::ExitManufacturerMode(MfgDisableMode mode) {
  if (!manufacturer_)
    return false;

  manufacturer_ = false;

  auto packet = CommandPacket::New(kMfgModeChange, sizeof(MfgModeChangeCommandParams));
  auto params = packet->mutable_payload<MfgModeChangeCommandParams>();
  params->enable = pw::bluetooth::emboss::GenericEnableParam::DISABLE;
  params->disable_mode = mode;

  SendCommand(packet->view());
  auto evt_packet = WaitForEventPacket();
  if (!evt_packet || evt_packet->event_code() != bt::hci_spec::kCommandCompleteEventCode) {
    errorf("VendorHci: ExitManufacturerMode failed");
    return false;
  }

  return true;
}

void VendorHci::SendCommand(const bt::PacketView<bt::hci_spec::CommandHeader>& command) const {
  zx_status_t status = ctrl_->write(0, command.data().data(), command.size(), nullptr, 0);
  if (status < 0) {
    errorf("VendorHci: SendCommand failed: %s", zx_status_get_string(status));
  }
}

std::unique_ptr<bt::hci::EventPacket> VendorHci::WaitForEventPacket(
    zx::duration timeout, bt::hci_spec::EventCode expected_event) const {
  zx_wait_item_t wait_items[2];
  uint32_t wait_item_count = 1;

  ZX_DEBUG_ASSERT(ctrl_);
  wait_items[0].handle = ctrl_->get();
  wait_items[0].waitfor = ZX_CHANNEL_READABLE;
  wait_items[0].pending = 0;

  if (acl_) {
    wait_items[1].handle = acl_->get();
    wait_items[1].waitfor = ZX_CHANNEL_READABLE;
    wait_items[1].pending = 0;
    wait_item_count++;
  }

  auto begin = zx::clock::get_monotonic();
  for (zx::duration elapsed; elapsed < timeout; elapsed = zx::clock::get_monotonic() - begin) {
    zx_status_t status = zx_object_wait_many(wait_items, wait_item_count,
                                             zx::deadline_after(timeout - elapsed).get());
    if (status != ZX_OK) {
      errorf("VendorHci: channel error: %s", zx_status_get_string(status));
      return nullptr;
    }

    // Determine which channel caused the event.
    zx_handle_t evt_handle = 0;
    for (unsigned i = 0; i < wait_item_count; ++i) {
      if (wait_items[i].pending & ZX_CHANNEL_READABLE) {
        evt_handle = wait_items[0].handle;
        break;
      }
    }

    ZX_DEBUG_ASSERT(evt_handle);

    // Allocate a buffer for the event. We don't know the size
    // beforehand we allocate the largest possible buffer.
    auto packet = bt::hci::EventPacket::New(bt::hci_spec::kMaxCommandPacketPayloadSize);
    if (!packet) {
      errorf("VendorHci: Failed to allocate event packet!");
      return nullptr;
    }

    uint32_t read_size;
    auto packet_bytes = packet->mutable_view()->mutable_data();
    zx_status_t read_status = zx_channel_read(evt_handle, 0u, packet_bytes.mutable_data(), nullptr,
                                              packet_bytes.size(), 0, &read_size, nullptr);
    if (read_status < 0) {
      errorf("VendorHci: Failed to read event bytes: %sn", zx_status_get_string(read_status));
      return nullptr;
    }

    if (read_size < sizeof(bt::hci_spec::EventHeader)) {
      errorf("VendorHci: Malformed event packet expected >%zu bytes, got %d",
             sizeof(bt::hci_spec::EventHeader), read_size);
      return nullptr;
    }

    // Compare the received payload size to what is in the header.
    const size_t rx_payload_size = read_size - sizeof(bt::hci_spec::EventHeader);
    const size_t size_from_header = packet->view().header().parameter_total_size;
    if (size_from_header != rx_payload_size) {
      errorf(
          "VendorHci: Malformed event packet - header payload size (%zu) != "
          "received (%zu)",
          size_from_header, rx_payload_size);
      return nullptr;
    }

    if (expected_event && expected_event != packet->event_code()) {
      tracef("VendorHci: keep waiting (expected: 0x%02x, got: 0x%02x)", expected_event,
             packet->event_code());
      continue;
    }

    packet->InitializeFromBuffer();
    return packet;
  }

  errorf("VendorHci: timed out waiting for event");
  return nullptr;
}

}  // namespace btintel
