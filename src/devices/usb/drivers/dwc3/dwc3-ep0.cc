// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.usb.descriptor/cpp/wire.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/fit/defer.h>

#include <mutex>

#include "src/devices/usb/drivers/dwc3/dwc3.h"

namespace dwc3 {

namespace fdescriptor = fuchsia_hardware_usb_descriptor;

zx_status_t Dwc3::Ep0Init() {
  if (zx::result result = ep0_.shared_fifo.Init(bti_); result.is_error()) {
    return result.error_value();
  }

  const std::array eps{&ep0_.out, &ep0_.in};
  for (Endpoint* ep : eps) {
    ep->max_packet_size = kEp0MaxPacketSize;
    ep->type = USB_ENDPOINT_CONTROL;
    ep->interval = 0;
  }

  return ZX_OK;
}

void Dwc3::Ep0Start() {
  CmdStartNewConfig(ep0_.out, 0);
  EpSetConfig(ep0_.out, true);
  EpSetConfig(ep0_.in, true);

  Ep0QueueSetup();
}

void Dwc3::Ep0QueueSetup() {
  CacheFlushInvalidate(ep0_.buffer.get(), 0, sizeof(fdescriptor::wire::UsbSetup));
  EpStartTransfer(ep0_.out, ep0_.shared_fifo, TRB_TRBCTL_SETUP, ep0_.buffer->phys(),
                  sizeof(fdescriptor::wire::UsbSetup));
  ep0_.state = Ep0::State::Setup;
}

void Dwc3::Ep0StartEndpoints() {
  FDF_LOG(DEBUG, "Dwc3::Ep0StartEndpoints");

  ep0_.in.type = USB_ENDPOINT_CONTROL;
  ep0_.in.interval = 0;
  CmdEpSetConfig(ep0_.in, true);

  // TODO(johngro): Why do we pass a hardcoded value of 2 for the resource ID
  // here?  Eventually, it is going to end up in the Params field of the DEPCMD
  // (Device EndPoint Command) register, where according to DWC docs (Table
  // 1-102), it will be ignored by the Start New Configuration command we are
  // sending.
  CmdStartNewConfig(ep0_.out, 2);
}

void Dwc3::HandleEp0TransferCompleteEvent(uint8_t ep_num) {
  ZX_DEBUG_ASSERT(is_ep0_num(ep_num));

  // Only DataOut state needs TRB read.
  dwc3_trb_t trb = ep0_.state == Ep0::State::DataOut ? ep0_.shared_fifo.Read() : dwc3_trb_t{};
  ep0_.shared_fifo.AdvanceRead();

  switch (ep0_.state) {
    case Ep0::State::Setup: {
      // Control Endpoint stall is cleared upon receiving SETUP.
      ep0_.out.stalled = false;

      memcpy(&ep0_.cur_setup, ep0_.buffer->virt(), sizeof(ep0_.cur_setup));

      FDF_LOG(DEBUG, "got setup: type: 0x%02X req: %d value: %d index: %d length: %d",
              ep0_.cur_setup.bm_request_type, ep0_.cur_setup.b_request, ep0_.cur_setup.w_value,
              ep0_.cur_setup.w_index, ep0_.cur_setup.w_length);

      const bool is_three_stage = ep0_.cur_setup.w_length > 0;
      const bool is_out = ((ep0_.cur_setup.bm_request_type & USB_DIR_MASK) == USB_DIR_OUT);
      if (is_three_stage && is_out) {
        CacheFlushInvalidate(ep0_.buffer.get(), 0, ep0_.buffer->size());
        EpStartTransfer(ep0_.out, ep0_.shared_fifo, TRB_TRBCTL_CONTROL_DATA, ep0_.buffer->phys(),
                        ep0_.buffer->size());
        ep0_.state = Ep0::State::DataOut;
        break;
      }

      ep0_.state = is_three_stage ? Ep0::State::DataIn : Ep0::State::WaitNrdyIn;
      HandleEp0Setup(is_three_stage ? ep0_.buffer->size() : 0);
      break;
    }
    case Ep0::State::DataOut: {
      ZX_DEBUG_ASSERT(ep_num == kEp0Out);
      zx_off_t received = ep0_.buffer->size() - TRB_BUFSIZ(trb.status);
      ep0_.state = Ep0::State::WaitNrdyIn;
      HandleEp0Setup(received);
      break;
    }
    case Ep0::State::DataIn:
      ZX_DEBUG_ASSERT(ep_num == kEp0In);
      ep0_.state = Ep0::State::WaitNrdyOut;
      break;
    case Ep0::State::Status:
      Ep0QueueSetup();
      break;
    default:
      break;
  }
}

void Dwc3::HandleEp0TransferNotReadyEvent(uint8_t ep_num, uint32_t stage) {
  ZX_DEBUG_ASSERT(is_ep0_num(ep_num));

  switch (ep0_.state) {
    case Ep0::State::Setup:
      if ((stage == DEPEVT_XFER_NOT_READY_STAGE_DATA) ||
          (stage == DEPEVT_XFER_NOT_READY_STAGE_STATUS)) {
        // Stall if we receive xfer not ready data/status while waiting for setup to complete
        ep0_.shared_fifo.Clear();
        EpSetStall(ep0_.out, true);
        Ep0QueueSetup();
      }
      break;
    case Ep0::State::DataOut:
      if ((ep_num == kEp0In) && (stage == DEPEVT_XFER_NOT_READY_STAGE_DATA)) {
        // end transfer and stall if we receive xfer not ready in the opposite direction
        ep0_.shared_fifo.Clear();
        CmdEpEndTransfer(ep0_.out);
        EpSetStall(ep0_.out, true);
        Ep0QueueSetup();
      }
      break;
    case Ep0::State::DataIn:
      if ((ep_num == kEp0Out) && (stage == DEPEVT_XFER_NOT_READY_STAGE_DATA)) {
        // end transfer and stall if we receive xfer not ready in the opposite direction
        ep0_.shared_fifo.Clear();
        CmdEpEndTransfer(ep0_.in);
        EpSetStall(ep0_.out, true);
        Ep0QueueSetup();
      }
      break;
    case Ep0::State::WaitNrdyOut:
      if (ep_num == kEp0Out) {
        EpStartTransfer(ep0_.out, ep0_.shared_fifo,
                        ep0_.cur_setup.w_length ? TRB_TRBCTL_STATUS_3 : TRB_TRBCTL_STATUS_2, 0, 0);
        ep0_.state = Ep0::State::Status;
      }
      break;
    case Ep0::State::WaitNrdyIn:
      if (ep_num == kEp0In) {
        EpStartTransfer(ep0_.in, ep0_.shared_fifo,
                        ep0_.cur_setup.w_length ? TRB_TRBCTL_STATUS_3 : TRB_TRBCTL_STATUS_2, 0, 0);
        ep0_.state = Ep0::State::Status;
      }
      break;
    case Ep0::State::Status:
    default:
      FDF_LOG(ERROR, "ready unhandled state %u", static_cast<uint32_t>(ep0_.state));
      break;
  }
}

void Dwc3::HandleEp0Setup(size_t length) {
  if (ep0_.cur_setup.bm_request_type == (USB_DIR_OUT | USB_TYPE_STANDARD | USB_RECIP_DEVICE)) {
    // handle some special setup requests in this driver
    switch (ep0_.cur_setup.b_request) {
      case USB_REQ_SET_ADDRESS:
        SetDeviceAddress(ep0_.cur_setup.w_value);
        return;
      case USB_REQ_SET_CONFIGURATION:
        ResetConfiguration();
        Ep0StartEndpoints();
        length = 0;
        break;
      default:
        // fall through to the common DoControlCall
        break;
    }
  }

  auto fail = [this]() {
    ep0_.shared_fifo.Clear();
    EpSetStall(ep0_.out, true);
    Ep0QueueSetup();
  };
  if (!dci_intf_.is_valid()) {
    fail();
    return;
  }

  const bool is_out = (ep0_.cur_setup.bm_request_type & USB_DIR_MASK) == USB_DIR_OUT;
  fidl::Arena arena;
  dci_intf_.buffer(arena)
      ->Control(ep0_.cur_setup, is_out
                                    ? fidl::VectorView<uint8_t>::FromExternal(
                                          reinterpret_cast<uint8_t*>(ep0_.buffer->virt()), length)
                                    : fidl::VectorView<uint8_t>::FromExternal(nullptr, 0))
      .Then(
          [this, is_out, fail, length](
              fidl::WireUnownedResult<fuchsia_hardware_usb_dci::UsbDciInterface::Control>& result) {
            if (!result.ok()) {
              FDF_LOG(ERROR, "(framework) Control(): %s", result.status_string());
              fail();
              return;
            }
            if (result->is_error()) {
              FDF_LOG(ERROR, "Control(): %s", zx_status_get_string(result->error_value()));
              fail();
              return;
            }

            if (!is_out) {
              // A lightweight byte-span is used to make it easier to process the read data.
              cpp20::span<uint8_t> read_data{result.value()->read.get()};
              // Don't blow out caller's buffer.
              if (read_data.size_bytes() > length) {
                fail();
                return;
              }

              if (!read_data.empty()) {
                std::memcpy(ep0_.buffer->virt(), read_data.data(), read_data.size_bytes());
              }

              FDF_LOG(DEBUG, "HandleSetup success: actual %zu", read_data.size_bytes());
              // queue a write for the data phase
              CacheFlush(ep0_.buffer.get(), 0, read_data.size_bytes());
              EpStartTransfer(ep0_.in, ep0_.shared_fifo, TRB_TRBCTL_CONTROL_DATA,
                              ep0_.buffer->phys(), read_data.size_bytes());
            }
          });
}

}  // namespace dwc3
