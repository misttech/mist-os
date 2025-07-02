// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/driver/logging/cpp/logger.h>

#include "src/devices/usb/drivers/dwc3/dwc3-regs.h"
#include "src/devices/usb/drivers/dwc3/dwc3.h"

namespace dwc3 {

void Dwc3::EpEnable(Endpoint& ep, bool enable) {
  std::lock_guard<std::mutex> lock(lock_);
  auto* mmio = get_mmio();

  if (enable) {
    DALEPENA::Get().ReadFrom(mmio).EnableEp(ep.ep_num).WriteTo(mmio);
  } else {
    DALEPENA::Get().ReadFrom(mmio).DisableEp(ep.ep_num).WriteTo(mmio);
  }

  ep.enabled = enable;
}

void Dwc3::EpSetConfig(Endpoint& ep, bool enable) {
  FDF_LOG(DEBUG, "Dwc3::EpSetConfig %u", ep.ep_num);

  if (enable) {
    CmdEpSetConfig(ep, false);
    CmdEpTransferConfig(ep);
    EpEnable(ep, true);
  } else {
    EpEnable(ep, false);
  }
}

zx_status_t Dwc3::EpSetStall(Endpoint& ep, bool stall) {
  if (!ep.enabled) {
    return ZX_ERR_BAD_STATE;
  }

  if (stall && !ep.stalled) {
    CmdEpSetStall(ep);
  } else if (!stall && ep.stalled) {
    CmdEpClearStall(ep);
  }

  ep.stalled = stall;
  return ZX_OK;
}

void Dwc3::EpStartTransfer(Endpoint& ep, TrbFifo& fifo, uint32_t type, zx_paddr_t buffer,
                           size_t length) {
  FDF_LOG(DEBUG, "Dwc3::EpStartTransfer ep %u type %u length %zu", ep.ep_num, type, length);

  dwc3_trb_t* trb = fifo.AdvanceWrite();
  trb->ptr_low = static_cast<uint32_t>(buffer);
  trb->ptr_high = static_cast<uint32_t>(buffer >> 32);
  trb->status = TRB_BUFSIZ(static_cast<uint32_t>(length));
  trb->control = type | TRB_LST | TRB_IOC | TRB_HWO;
  zx_paddr_t trb_phys = fifo.Write(trb);

  CmdEpStartTransfer(ep, trb_phys);
}

void Dwc3::EpServer::CancelAll(zx_status_t reason) {
  std::queue<usb::RequestVariant> to_cancel;
  {
    std::lock_guard<std::mutex> _(uep_->ep.lock);
    to_cancel = std::move(queued_reqs);
    if (current_req.has_value()) {
      dwc3_->CmdEpEndTransfer(uep_->ep);
      to_cancel.push(std::move(*current_req));
      current_req.reset();
    }
    uep_->fifo.Clear();
  }

  for (; !to_cancel.empty(); to_cancel.pop()) {
    RequestComplete(reason, 0, std::move(to_cancel.front()));
  }
}

// No thread safety analysis because UserEpQueueNext already holds uep.ep.lock, which is equivalent
// to uep.server->uep.ep.lock
void Dwc3::UserEpQueueNext(UserEndpoint& uep) __TA_NO_THREAD_SAFETY_ANALYSIS {
  if (uep.server->current_req.has_value() || !uep.ep.got_not_ready ||
      uep.server->queued_reqs.empty()) {
    return;
  }

  uep.server->current_req.emplace(std::move(uep.server->queued_reqs.front()));
  uep.server->queued_reqs.pop();

  zx::result result = uep.server->get_iter(*uep.server->current_req, zx_system_get_page_size());
  ZX_ASSERT_MSG(result.is_ok(), "[BUG] server->phys_iter(): %s", result.status_string());

  // TODO(voydanoff) scatter/gather support
  zx_paddr_t phys;
  size_t size;
  std::tie(phys, size) = *result->at(0).begin();
  EpStartTransfer(uep.ep, uep.fifo, TRB_TRBCTL_NORMAL, phys, size);
}

void Dwc3::HandleEpTransferCompleteEvent(uint8_t ep_num) __TA_NO_THREAD_SAFETY_ANALYSIS {
  if (is_ep0_num(ep_num)) {
    HandleEp0TransferCompleteEvent(ep_num);
    return;
  }

  std::optional<usb::RequestVariant> req;
  uint32_t actual;

  // No thread safety analysis because this already holds uep.ep.lock, which is equivalent
  // to uep.server->uep.ep.lock
  UserEndpoint* const uep = get_user_endpoint(ep_num);
  ZX_DEBUG_ASSERT(uep != nullptr);
  {
    std::lock_guard<std::mutex> lock{uep->ep.lock};
    if (!uep->server->current_req.has_value()) {
      FDF_LOG(ERROR, "no usb request found to complete!");
      return;
    }
    dwc3_trb_t trb = uep->fifo.Read();

    if (trb.control & TRB_HWO) {
      FDF_LOG(ERROR, "TRB_HWO still set in dwc3_ep_xfer_complete %d", uep->ep.ep_num);
      return;
    }

    req.emplace(std::move(*uep->server->current_req));
    uep->server->current_req.reset();
    uep->fifo.AdvanceRead();
    actual =
        std::get<usb::FidlRequest>(*req)->data()->at(0).size().value() - TRB_BUFSIZ(trb.status);
  }

  uep->server->RequestComplete(ZX_OK, actual, std::move(*req));
}

void Dwc3::HandleEpTransferNotReadyEvent(uint8_t ep_num, uint32_t stage) {
  if (is_ep0_num(ep_num)) {
    HandleEp0TransferNotReadyEvent(ep_num, stage);
    return;
  }

  UserEndpoint* const uep = get_user_endpoint(ep_num);
  ZX_DEBUG_ASSERT(uep != nullptr);

  std::lock_guard<std::mutex> lock(uep->ep.lock);
  uep->ep.got_not_ready = true;
  UserEpQueueNext(*uep);
}

void Dwc3::HandleEpTransferStartedEvent(uint8_t ep_num, uint32_t rsrc_id) {
  if (is_ep0_num(ep_num)) {
    std::lock_guard<std::mutex> ep0_lock(ep0_.lock);
    ((ep_num == kEp0Out) ? ep0_.out : ep0_.in).rsrc_id = rsrc_id;
  } else {
    UserEndpoint* const uep = get_user_endpoint(ep_num);
    ZX_DEBUG_ASSERT(uep != nullptr);

    std::lock_guard<std::mutex> lock(uep->ep.lock);
    uep->ep.rsrc_id = rsrc_id;
  }
}

}  // namespace dwc3
