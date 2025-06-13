// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/driver/logging/cpp/logger.h>
#include <lib/fit/defer.h>

#include "src/devices/usb/drivers/dwc3/dwc3-regs.h"
#include "src/devices/usb/drivers/dwc3/dwc3-types.h"
#include "src/devices/usb/drivers/dwc3/dwc3.h"

namespace dwc3 {

void Dwc3::HandleEpEvent(uint32_t event) {
  const uint32_t type = DEPEVT_TYPE(event);
  const uint8_t ep_num = DEPEVT_PHYS_EP(event);
  const uint32_t status = DEPEVT_STATUS(event);

  switch (type) {
    case DEPEVT_XFER_COMPLETE:
      FDF_LOG(DEBUG, "ep[%u] DEPEVT_XFER_COMPLETE", ep_num);
      HandleEpTransferCompleteEvent(ep_num);
      break;
    case DEPEVT_XFER_IN_PROGRESS:
      FDF_LOG(DEBUG, "ep[%u] DEPEVT_XFER_IN_PROGRESS: status %u", ep_num, status);
      break;
    case DEPEVT_XFER_NOT_READY:
      FDF_LOG(DEBUG, "ep[%u] DEPEVT_XFER_NOT_READY", ep_num);
      HandleEpTransferNotReadyEvent(ep_num, DEPEVT_XFER_NOT_READY_STAGE(event));
      break;
    case DEPEVT_STREAM_EVT:
      FDF_LOG(DEBUG, "ep[%u] DEPEVT_STREAM_EVT ep_num: status %u", ep_num, status);
      break;
    case DEPEVT_CMD_CMPLT: {
      uint32_t cmd_type = DEPEVT_CMD_CMPLT_CMD_TYPE(event);
      uint32_t rsrc_id = DEPEVT_CMD_CMPLT_RSRC_ID(event);
      FDF_LOG(DEBUG, "ep[%u] DEPEVT_CMD_COMPLETE: type %u rsrc_id %u", ep_num, cmd_type, rsrc_id);
      if (cmd_type == DEPCMD::DEPSTRTXFER) {
        HandleEpTransferStartedEvent(ep_num, rsrc_id);
      }
      break;
    }
    default:
      FDF_LOG(ERROR, "dwc3_handle_ep_event: unknown event type %u", type);
      break;
  }
}

void Dwc3::HandleEvent(uint32_t event) {
  if (!(event & DEPEVT_NON_EP)) {
    HandleEpEvent(event);
    return;
  }

  uint32_t type = DEVT_TYPE(event);
  uint32_t info = DEVT_INFO(event);

  switch (type) {
    case DEVT_DISCONNECT:
      FDF_LOG(DEBUG, "DEVT_DISCONNECT");
      break;
    case DEVT_USB_RESET:
      FDF_LOG(DEBUG, "DEVT_USB_RESET");
      HandleResetEvent();
      break;
    case DEVT_CONNECTION_DONE:
      FDF_LOG(DEBUG, "DEVT_CONNECTION_DONE");
      HandleConnectionDoneEvent();
      break;
    case DEVT_LINK_STATE_CHANGE:
      FDF_LOG(DEBUG, "DEVT_LINK_STATE_CHANGE: ");
      switch (info) {
        case DSTS::USBLNKST_U0 | DEVT_LINK_STATE_CHANGE_SS:
          FDF_LOG(DEBUG, "DSTS::USBLNKST_U0");
          break;
        case DSTS::USBLNKST_U1 | DEVT_LINK_STATE_CHANGE_SS:
          FDF_LOG(DEBUG, "DSTS_USBLNKST_U1");
          break;
        case DSTS::USBLNKST_U2 | DEVT_LINK_STATE_CHANGE_SS:
          FDF_LOG(DEBUG, "DSTS_USBLNKST_U2");
          break;
        case DSTS::USBLNKST_U3 | DEVT_LINK_STATE_CHANGE_SS:
          FDF_LOG(DEBUG, "DSTS_USBLNKST_U3");
          break;
        case DSTS::USBLNKST_ESS_DIS | DEVT_LINK_STATE_CHANGE_SS:
          FDF_LOG(DEBUG, "DSTS_USBLNKST_ESS_DIS");
          break;
        case DSTS::USBLNKST_RX_DET | DEVT_LINK_STATE_CHANGE_SS:
          FDF_LOG(DEBUG, "DSTS_USBLNKST_RX_DET");
          break;
        case DSTS::USBLNKST_ESS_INACT | DEVT_LINK_STATE_CHANGE_SS:
          FDF_LOG(DEBUG, "DSTS_USBLNKST_ESS_INACT");
          break;
        case DSTS::USBLNKST_POLL | DEVT_LINK_STATE_CHANGE_SS:
          FDF_LOG(DEBUG, "DSTS_USBLNKST_POLL");
          break;
        case DSTS::USBLNKST_RECOV | DEVT_LINK_STATE_CHANGE_SS:
          FDF_LOG(DEBUG, "DSTS_USBLNKST_RECOV");
          break;
        case DSTS::USBLNKST_HRESET | DEVT_LINK_STATE_CHANGE_SS:
          FDF_LOG(DEBUG, "DSTS_USBLNKST_HRESET");
          break;
        case DSTS::USBLNKST_CMPLY | DEVT_LINK_STATE_CHANGE_SS:
          FDF_LOG(DEBUG, "DSTS_USBLNKST_CMPLY");
          break;
        case DSTS::USBLNKST_LPBK | DEVT_LINK_STATE_CHANGE_SS:
          FDF_LOG(DEBUG, "DSTS_USBLNKST_LPBK");
          break;
        case DSTS::USBLNKST_RESUME_RESET | DEVT_LINK_STATE_CHANGE_SS:
          FDF_LOG(DEBUG, "DSTS_USBLNKST_RESUME_RESET");
          break;
        case DSTS::USBLNKST_ON:
          FDF_LOG(DEBUG, "DSTS_USBLNKST_ON");
          break;
        case DSTS::USBLNKST_SLEEP:
          FDF_LOG(DEBUG, "DSTS_USBLNKST_SLEEP");
          break;
        case DSTS::USBLNKST_SUSPEND:
          FDF_LOG(DEBUG, "DSTS_USBLNKST_SUSPEND");
          break;
        case DSTS::USBLNKST_DISCONNECTED:
          FDF_LOG(DEBUG, "DSTS_USBLNKST_DISCONNECTED");
          break;
        case DSTS::USBLNKST_EARLY_SUSPEND:
          FDF_LOG(DEBUG, "DSTS_USBLNKST_EARLY_SUSPEND");
          break;
        case DSTS::USBLNKST_RESET:
          FDF_LOG(DEBUG, "DSTS_USBLNKST_RESET");
          break;
        case DSTS::USBLNKST_RESUME:
          FDF_LOG(DEBUG, "DSTS_USBLNKST_RESUME");
          break;
        default:
          FDF_LOG(ERROR, "unknown state %d", info);
          break;
      }
      break;
    case DEVT_REMOTE_WAKEUP:
      FDF_LOG(DEBUG, "DEVT_REMOTE_WAKEUP");
      break;
    case DEVT_HIBERNATE_REQUEST:
      FDF_LOG(DEBUG, "DEVT_HIBERNATE_REQUEST");
      break;
    case DEVT_SUSPEND_ENTRY:
      FDF_LOG(DEBUG, "DEVT_SUSPEND_ENTRY");
      // TODO(voydanoff) is this the best way to detect disconnect?
      HandleDisconnectedEvent();
      break;
    case DEVT_SOF:
      FDF_LOG(DEBUG, "DEVT_SOF");
      break;
    case DEVT_ERRATIC_ERROR:
      FDF_LOG(DEBUG, "DEVT_ERRATIC_ERROR");
      break;
    case DEVT_COMMAND_COMPLETE:
      FDF_LOG(DEBUG, "DEVT_COMMAND_COMPLETE");
      break;
    case DEVT_EVENT_BUF_OVERFLOW:
      FDF_LOG(DEBUG, "DEVT_EVENT_BUF_OVERFLOW");
      break;
    case DEVT_VENDOR_TEST_LMP:
      FDF_LOG(DEBUG, "DEVT_VENDOR_TEST_LMP");
      break;
    case DEVT_STOPPED_DISCONNECT:
      FDF_LOG(DEBUG, "DEVT_STOPPED_DISCONNECT");
      break;
    case DEVT_L1_RESUME_DETECT:
      FDF_LOG(DEBUG, "DEVT_L1_RESUME_DETECT");
      break;
    case DEVT_LDM_RESPONSE:
      FDF_LOG(DEBUG, "DEVT_LDM_RESPONSE");
      break;
    default:
      FDF_LOG(ERROR, "dwc3_handle_event: unknown event type %u", type);
      break;
  }
}

void Dwc3::CompletePendingRequests() {
  while (!pending_completions_.empty()) {
    std::optional<RequestInfo> info{pending_completions_.pop()};
    info->uep->server->RequestComplete(info->status, info->actual, std::move(info->req));
  }
}

void Dwc3::HandleIrq(async_dispatcher_t* dispatcher, async::IrqBase* irq, zx_status_t status,
                     const zx_packet_interrupt_t* interrupt) {
  auto* mmio = get_mmio();

  uint32_t event_bytes;
  while ((event_bytes = GEVNTCOUNT::Get(0).ReadFrom(mmio).EVNTCOUNT()) > 0) {
    uint32_t event_count = event_bytes / sizeof(uint32_t);
    event_fifo_.Read(event_count);

    for (uint32_t i = 0; i < event_count; i++) {
      HandleEvent(event_fifo_.Advance());
    }

    // acknowledge the events we have processed
    GEVNTCOUNT::Get(0).FromValue(0).set_EVNTCOUNT(event_bytes).WriteTo(mmio);
  }

  complete_pending_requests_.Post(dispatcher);
  irq_.ack();
}

void Dwc3::StartEvents() {
  auto* mmio = get_mmio();

  // set event buffer pointer and size
  // keep interrupts masked until we are ready
  zx_paddr_t paddr = event_fifo_.GetPhys();
  ZX_DEBUG_ASSERT(paddr != 0);

  GEVNTADR::Get(0).FromValue(0).set_EVNTADR(paddr).WriteTo(mmio);
  GEVNTSIZ::Get(0)
      .FromValue(0)
      .set_EVENTSIZ(EventFifo::kEventBufferSize)
      .set_EVNTINTRPTMASK(0)
      .WriteTo(mmio);
  GEVNTCOUNT::Get(0).FromValue(0).set_EVNTCOUNT(0).WriteTo(mmio);

  // enable events
  DEVTEN::Get()
      .FromValue(0)
      .set_L1SUSPEN(1)
      .set_U3L2L1SuspEn(1)
      .set_CONNECTDONEEVTEN(1)
      .set_USBRSTEVTEN(1)
      .set_DISSCONNEVTEN(1)
      .WriteTo(mmio);
}

}  // namespace dwc3
