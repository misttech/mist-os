// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/usb/drivers/xhci/xhci-endpoint.h"

#include <lib/fit/defer.h>

#include "src/devices/usb/drivers/xhci/usb-xhci.h"

namespace usb_xhci {

Endpoint::Endpoint(UsbXhci* hci, uint32_t device_id, uint8_t address)
    : usb_endpoint::UsbEndpoint(hci->bti(), address), hci_(hci), device_id_(device_id) {
  loop_.StartThread("endpoint-thread");
}

zx_status_t Endpoint::Init(EventRing* event_ring, fdf::MmioBuffer* mmio) {
  return transfer_ring_.Init(hci_->GetPageSize(), hci_->bti(), event_ring,
                             hci_->Is32BitController(), mmio, hci_);
}

void Endpoint::OnUnbound(fidl::UnbindInfo info,
                         fidl::ServerEnd<fuchsia_hardware_usb_endpoint::Endpoint> server_end) {
  if (hci_->Running()) {
    auto status = hci_->UsbHciCancelAll(device_id_, ep_addr());
    if (status != ZX_OK) {
      zxlogf(ERROR, "Could not cancel all %d", status);
    }

    status = hci_->RunSynchronously(kPrimaryInterrupter,
                                    hci_->UsbHciDisableEndpoint(device_id_, ep_addr()));
    if (status != ZX_OK) {
      zxlogf(ERROR, "Could not disable endpoint %d", status);
    }
  }

  usb_endpoint::UsbEndpoint::OnUnbound(info, std::move(server_end));
}

void Endpoint::QueueRequests(QueueRequestsRequest& request,
                             QueueRequestsCompleter::Sync& completer) {
  for (auto& req : request.req()) {
    QueueRequest(usb::FidlRequest{std::move(req)});
  }
  uint8_t index = static_cast<uint8_t>(XhciEndpointIndex(ep_addr()) - 1);
  hci_->RingDoorbell(hci_->GetDeviceState()[device_id_]->GetSlot(), 2 + index);
}

void Endpoint::CancelAll(CancelAllCompleter::Sync& completer) {
  auto status = hci_->UsbHciCancelAll(device_id_, ep_addr());
  if (status != ZX_OK) {
    completer.Reply(fit::as_error(status));
    return;
  }
  completer.Reply(fit::ok());
}

void Endpoint::QueueRequest(usb::FidlRequest request) {
  if (!hci_->Running()) {
    RequestComplete(ZX_ERR_IO_NOT_PRESENT, 0, std::move(request));
    return;
  }
  {
    fbl::AutoLock _(&hci_->GetDeviceState()[device_id_]->transaction_lock());
    if (!hci_->GetDeviceState()[device_id_]->GetSlot()) {
      RequestComplete(ZX_ERR_IO_NOT_PRESENT, 0, std::move(request));
      return;
    }
  }
  if (unlikely(ep_addr() == 0)) {
    RequestComplete(ZX_ERR_NOT_SUPPORTED, 0, std::move(request));
    return;
  }
  NormalRequestQueue(std::move(request));
}

void Endpoint::NormalRequestQueue(usb_endpoint::RequestVariant request) {
  UsbRequestState pending_transfer;
  uint8_t index = static_cast<uint8_t>(XhciEndpointIndex(ep_addr()) - 1);
  fbl::AutoLock transaction_lock(&hci_->GetDeviceState()[device_id_]->transaction_lock());
  if (hci_->GetDeviceState()[device_id_]->IsDisconnecting()) {
    transaction_lock.release();
    RequestComplete(ZX_ERR_IO_NOT_PRESENT, 0, std::move(request));
    return;
  }
  if (transfer_ring_.stalled()) {
    transaction_lock.release();
    RequestComplete(ZX_ERR_IO_REFUSED, 0, std::move(request));
    return;
  }
  auto* control =
      reinterpret_cast<uint32_t*>(hci_->GetDeviceState()[device_id_]->GetInputContext()->virt());
  auto* endpoint_context = reinterpret_cast<EndpointContext*>(
      reinterpret_cast<unsigned char*>(control) + (hci_->slot_size_bytes() * (2 + (index + 1))));
  if (!transfer_ring_.active()) {
    transaction_lock.release();
    RequestComplete(ZX_ERR_INTERNAL, 0, std::move(request));
    return;
  }
  pending_transfer.burst_size = endpoint_context->MaxBurstSize() + 1;
  pending_transfer.max_packet_size = endpoint_context->MAX_PACKET_SIZE();
  pending_transfer.context = transfer_ring_.AllocateContext();
  if (!pending_transfer.context) {
    transaction_lock.release();
    RequestComplete(ZX_ERR_NO_MEMORY, 0, std::move(request));
    return;
  }
  pending_transfer.context->request.emplace(std::move(request));

  if (transfer_ring_.IsIsochronous()) {
    // Isoc endpoints of the default interface (i.e. b_alternate_setting=0) are required to have a
    // w_max_packet_size=0 to prevent enumerated devices from reserving bandwidth by default. We'll
    // consider any request for a zero-length pipe as invalid (see USB 2.0 spec. 5.6.2).
    if (pending_transfer.max_packet_size == 0) {
      transaction_lock.release();
      RequestComplete(ZX_ERR_INVALID_ARGS, 0, std::move(*pending_transfer.context->request));
      return;
    }

    // Release the lock while we're sleeping to avoid blocking
    // other operations.
    hci_->GetDeviceState()[device_id_]->transaction_lock().Release();
    auto status = WaitForIsochronousReady(
        std::holds_alternative<usb::FidlRequest>(*pending_transfer.context->request)
            ? *std::get<usb::FidlRequest>(*pending_transfer.context->request)
                   .request()
                   .information()
                   ->isochronous()
                   ->frame_id()
            : std::get<Request>(*pending_transfer.context->request).request()->header.frame);
    hci_->GetDeviceState()[device_id_]->transaction_lock().Acquire();
    if (status != ZX_OK) {
      transaction_lock.release();
      RequestComplete(status, 0, std::move(*pending_transfer.context->request));
      return;
    }

    // Check again since we've re-acquired lock
    if (hci_->GetDeviceState()[device_id_]->IsDisconnecting()) {
      transaction_lock.release();
      RequestComplete(ZX_ERR_IO_NOT_PRESENT, 0, std::move(*pending_transfer.context->request));
      return;
    }
    if (transfer_ring_.stalled()) {
      transaction_lock.release();
      RequestComplete(ZX_ERR_IO_REFUSED, 0, std::move(*pending_transfer.context->request));
      return;
    }
    if (!transfer_ring_.active()) {
      transaction_lock.release();
      RequestComplete(ZX_ERR_INTERNAL, 0, std::move(*pending_transfer.context->request));
      return;
    }
  }

  // Start the transaction
  pending_transfer.transaction = transfer_ring_.SaveState();
  auto rollback_transaction = [&]() __TA_NO_THREAD_SAFETY_ANALYSIS {
    transfer_ring_.Restore(pending_transfer.transaction);
  };
  auto status = StartNormalTransaction(
      &pending_transfer,
      static_cast<uint8_t>(hci_->GetDeviceState()[device_id_]->GetInterrupterTarget()));
  if (status != ZX_OK) {
    rollback_transaction();
    transaction_lock.release();
    RequestComplete(status, 0, std::move(*pending_transfer.context->request));
    return;
  }
  // Continue the transaction
  status = ContinueNormalTransaction(&pending_transfer);
  if (status != ZX_OK) {
    rollback_transaction();
    transaction_lock.release();
    RequestComplete(status, 0, std::move(*pending_transfer.context->request));
    return;
  }
  // Commit the transaction -- starting the actual transfer
  CommitNormalTransaction(&pending_transfer);
}

zx_status_t Endpoint::WaitForIsochronousReady(uint64_t target_frame) {
  // Cannot schedule more than 895 ms into the future per section 4.11.2.5
  // in the xHCI specification (revision 1.2)
  constexpr int kMaxSchedulingInterval = 895;
  if (target_frame) {
    uint64_t frame = hci_->UsbHciGetCurrentFrame();
    while (static_cast<int32_t>(target_frame - frame) > kMaxSchedulingInterval) {
      uint32_t time = static_cast<uint32_t>((target_frame - frame) - kMaxSchedulingInterval);
      zx::nanosleep(zx::deadline_after(zx::msec(time)));
      frame = hci_->UsbHciGetCurrentFrame();
    }

    if (target_frame < frame) {
      return ZX_ERR_IO_MISSED_DEADLINE;
    }
  }
  return ZX_OK;
}

zx_status_t Endpoint::StartNormalTransaction(UsbRequestState* state, uint8_t interrupter_target) {
  size_t packet_count = 0;

  // Normal transfer
  auto status = std::visit([&](auto&& req) -> zx_status_t { return req.PhysMap(hci_->bti()); },
                           *state->context->request);
  if (status != ZX_OK) {
    return status;
  }
  size_t pending_len = std::holds_alternative<usb::FidlRequest>(*state->context->request)
                           ? std::get<usb::FidlRequest>(*state->context->request).length()
                           : std::get<Request>(*state->context->request).request()->header.length;
  uint32_t total_len = 0;
  auto iters = get_iter(*state->context->request, k64KiB);
  if (iters.is_error()) {
    return iters.error_value();
  }

  for (auto& iter : iters.value()) {
    for (auto [paddr, len] : iter) {
      if (len > pending_len) {
        len = pending_len;
      }
      if (!paddr) {
        break;
      }
      if (!len) {
        continue;
      }
      total_len += static_cast<uint32_t>(len);
      packet_count++;
      pending_len -= len;
    }
  }

  if (pending_len) {
    // Something doesn't add up here....
    return ZX_ERR_BAD_STATE;
  }
  // Allocate contiguous memory
  auto contig_trb_info = transfer_ring_.AllocateContiguous(packet_count);
  if (contig_trb_info.is_error()) {
    return contig_trb_info.error_value();
  }
  state->info = contig_trb_info.value();
  state->total_len = total_len;
  state->packet_count = packet_count;
  state->first_cycle = state->info.first()[0].status;
  state->first_trb = state->info.first().data();
  state->last_trb = state->info.trbs.data() + (packet_count - 1);
  state->interrupter = static_cast<uint8_t>(interrupter_target);

  return ZX_OK;
}

zx_status_t Endpoint::ContinueNormalTransaction(UsbRequestState* state) {
  // Data stage
  size_t pending_len = std::holds_alternative<usb::FidlRequest>(*state->context->request)
                           ? std::get<usb::FidlRequest>(*state->context->request).length()
                           : std::get<Request>(*state->context->request).request()->header.length;
  auto current_nop = state->info.nop.data();
  if (current_nop) {
    while (Control::FromTRB(current_nop).Type() == Control::Nop) {
      bool producer_cycle_state = current_nop->status;
      bool cycle = (current_nop == state->first_trb) ? !producer_cycle_state : producer_cycle_state;
      Control::FromTRB(current_nop).set_Cycle(cycle).ToTrb(current_nop);
      current_nop->status = 0;
      current_nop++;
    }
  }
  if (state->first_trb) {
    TRB* current = state->info.trbs.data();
    auto iters = get_iter(*state->context->request, k64KiB);
    if (iters.is_error()) {
      return iters.error_value();
    }

    for (auto& iter : iters.value()) {
      for (auto [paddr, len] : iter) {
        if (!len) {
          break;
        }
        len = std::min(len, pending_len);
        pending_len -= len;
        state->packet_count--;
        TRB* next = current + 1;
        if (next == state->last_trb + 1) {
          next = nullptr;
        }
        uint32_t pcs = current->status;
        current->status = 0;
        enum Control::Type type;
        if ((transfer_ring_.IsIsochronous() && state->first_trb == current)) {
          // Force direct mode as workaround for USB audio latency issue.
          type = Control::Isoch;
          Isoch* data = reinterpret_cast<Isoch*>(current);
          // Burst size is number of packets, not bytes
          uint32_t burst_size = state->burst_size;
          uint32_t packet_size = state->max_packet_size;
          uint32_t packet_count = state->total_len / packet_size;
          if (!packet_count) {
            packet_count = 1;
          }
          // Number of bursts - 1
          uint32_t burst_count = packet_count / burst_size;
          if (burst_count) {
            burst_count--;
          }
          // Zero-based last-burst-packet count (where 0 == 1 packet)
          uint32_t last_burst_packet_count = packet_count % burst_size;
          if (last_burst_packet_count) {
            last_burst_packet_count--;
          }
          auto frame_id = std::holds_alternative<usb::FidlRequest>(*state->context->request)
                              ? *std::get<usb::FidlRequest>(*state->context->request)
                                     .request()
                                     .information()
                                     ->isochronous()
                                     ->frame_id()
                              : std::get<Request>(*state->context->request).request()->header.frame;
          data->set_CHAIN(next != nullptr)
              .set_SIA(frame_id == 0)
              .set_TLBPC(last_burst_packet_count)
              .set_FrameID(frame_id % 2048)
              .set_TBC(burst_count)
              .set_INTERRUPTER(state->interrupter)
              .set_LENGTH(len)
              .set_SIZE(packet_count)
              .set_NO_SNOOP(!hci_->HasCoherentCache())
              .set_IOC(next == nullptr)
              .set_ISP(true);
        } else {
          type = Control::Normal;
          Normal* data = reinterpret_cast<Normal*>(current);
          data->set_CHAIN(next != nullptr)
              .set_INTERRUPTER(state->interrupter)
              .set_LENGTH(len)
              .set_SIZE(state->packet_count)
              .set_NO_SNOOP(!hci_->HasCoherentCache())
              .set_IOC(next == nullptr)
              .set_ISP(true);
        }

        current->ptr = paddr;
        Control::FromTRB(current)
            .set_Cycle(unlikely(current == state->first_trb) ? !pcs : pcs)
            .set_Type(type)
            .ToTrb(current);
        current = next;
      }
    }
  }
  return ZX_OK;
}

void Endpoint::CommitNormalTransaction(UsbRequestState* state) {
  hw_mb();
  // Start the transaction!
  bool is_banjo = std::holds_alternative<Request>(*state->context->request);
  if (is_banjo) {
    usb_request_cache_flush_invalidate(
        std::get<Request>(*state->context->request).request(), 0,
        std::get<Request>(*state->context->request).request()->header.length);
  }
  // In FIDL mode, we expect cache to be flushed already
  transfer_ring_.AssignContext(state->last_trb, std::move(state->context), state->first_trb);
  Control::FromTRB(state->first_trb).set_Cycle(state->first_cycle).ToTrb(state->first_trb);

  transfer_ring_.CommitTransaction(state->transaction);
  if (is_banjo) {
    uint8_t index = static_cast<uint8_t>(XhciEndpointIndex(ep_addr()) - 1);
    hci_->RingDoorbell(hci_->GetDeviceState()[device_id_]->GetSlot(), 2 + index);
  }
}

}  // namespace usb_xhci
