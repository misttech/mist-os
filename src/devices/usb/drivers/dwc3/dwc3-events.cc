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

int Dwc3::IrqThread() {
  auto* mmio = get_mmio();
  const uint32_t* const ring_start = static_cast<const uint32_t*>(event_buffer_->virt());
  const uint32_t* const ring_end = ring_start + (kEventBufferSize / sizeof(*ring_start));
  const uint32_t* ring_cur = ring_start;
  bool shutdown_now = false;

  while (!shutdown_now) {
    // Perform the callbacks for any requests which are pending completion.
    while (!pending_completions_.empty()) {
      std::optional<RequestInfo> info{pending_completions_.pop()};
      info->uep->server->RequestComplete(info->status, info->actual, std::move(info->req));
    }

    // Wait for a new interrupt.
    zx_port_packet_t wakeup_pkt;
    if (zx_status_t status = irq_port_.wait(zx::time::infinite(), &wakeup_pkt); status != ZX_OK) {
      FDF_LOG(ERROR, "Dwc3::IrqThread: zx_port_wait returned %s", zx_status_get_string(status));
      shutdown_now = true;
      continue;
    }

    // Was this an actual HW interrupt?  If so, process any new events in the
    // event buffer.
    if (wakeup_pkt.type == ZX_PKT_TYPE_INTERRUPT) {
      // Our interrupt should be edge triggered, so go ahead and ack and re-enable
      // it now so that we don't accidentally miss any new interrupts while
      // process these.
      irq_.ack();

      uint32_t event_count;
      while ((event_count = GEVNTCOUNT::Get(0).ReadFrom(mmio).EVNTCOUNT()) > 0) {
        // invalidate cache so we can read fresh events
        const zx_off_t offset = (ring_cur - ring_start) * sizeof(*ring_cur);
        const size_t todo = std::min<size_t>(ring_end - ring_cur, event_count);
        CacheFlushInvalidate(event_buffer_.get(), offset, todo * sizeof(*ring_cur));
        if (event_count > todo) {
          CacheFlushInvalidate(event_buffer_.get(), 0, (event_count - todo) * sizeof(*ring_cur));
        }

        for (uint32_t i = 0; i < event_count; i += sizeof(uint32_t)) {
          uint32_t event = *ring_cur++;
          if (ring_cur == ring_end) {
            ring_cur = ring_start;
          }

          HandleEvent(event);
        }

        // acknowledge the events we have processed
        GEVNTCOUNT::Get(0).FromValue(0).set_EVNTCOUNT(event_count).WriteTo(mmio);
      }
    } else if (wakeup_pkt.type == ZX_PKT_TYPE_USER) {
      const IrqSignal signal = GetIrqSignal(wakeup_pkt);
      switch (signal) {
        case IrqSignal::Wakeup:
          // Nothing to do here, just loop around and process the pending
          // completion queue.
          break;
        case IrqSignal::Exit:
          FDF_LOG(INFO, "Dwc3::IrqThread: shutting down");
          shutdown_now = true;
          break;
        default:
          FDF_LOG(ERROR, "Dwc3::IrqThread: got invalid signal value %u",
                  static_cast<std::underlying_type_t<decltype(signal)>>(signal));
          shutdown_now = true;
          break;
      }
      // TODO: b/377950112 - Determine whether a Wakeup user packet should lead to shutdown.
      shutdown_now = true;
      continue;
    } else {
      FDF_LOG(ERROR, "Dwc3::IrqThread: unrecognized packet type %u", wakeup_pkt.type);
      shutdown_now = true;
      continue;
    }
  }
  return 0;
}

void Dwc3::StartEvents() {
  auto* mmio = get_mmio();

  // set event buffer pointer and size
  // keep interrupts masked until we are ready
  zx_paddr_t paddr = event_buffer_->phys();
  ZX_DEBUG_ASSERT(paddr != 0);

  GEVNTADR::Get(0).FromValue(0).set_EVNTADR(paddr).WriteTo(mmio);
  GEVNTSIZ::Get(0).FromValue(0).set_EVENTSIZ(kEventBufferSize).set_EVNTINTRPTMASK(0).WriteTo(mmio);
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
