// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/driver/logging/cpp/logger.h>

#include "src/devices/usb/drivers/dwc3/dwc3-regs.h"
#include "src/devices/usb/drivers/dwc3/dwc3.h"

namespace dwc3 {

void Dwc3::EpEnable(const Endpoint& ep, bool enable) {
  auto* mmio = get_mmio();

  if (enable) {
    DALEPENA::Get().ReadFrom(mmio).EnableEp(ep.ep_num).WriteTo(mmio);
  } else {
    DALEPENA::Get().ReadFrom(mmio).DisableEp(ep.ep_num).WriteTo(mmio);
  }
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
  if (current_req.has_value()) {
    dwc3_->CmdEpEndTransfer(uep_->ep);
    RequestComplete(reason, 0, std::move(*current_req));
    current_req.reset();
  }

  for (; !queued_reqs.empty(); queued_reqs.pop()) {
    RequestComplete(reason, 0, std::move(queued_reqs.front()));
  }
}

void Dwc3::UserEpQueueNext(UserEndpoint& uep) {
  if (uep.server->current_req.has_value() || !uep.ep.got_not_ready ||
      uep.server->queued_reqs.empty()) {
    return;
  }

  uep.server->current_req.emplace(std::move(uep.server->queued_reqs.front()));
  uep.server->queued_reqs.pop();

  zx::result result{uep.server->get_iter(*uep.server->current_req, zx_system_get_page_size())};
  if (result.is_error()) {
    FDF_LOG(ERROR, "[BUG] server->phys_iter(): %s", result.status_string());
  }
  ZX_ASSERT(result.is_ok());

  // TODO(voydanoff) scatter/gather support
  zx_paddr_t phys;
  size_t size;
  std::tie(phys, size) = *result->at(0).begin();
  EpStartTransfer(uep.ep, uep.fifo, TRB_TRBCTL_NORMAL, phys, size);
}

void Dwc3::HandleEpTransferCompleteEvent(uint8_t ep_num) {
  if (is_ep0_num(ep_num)) {
    HandleEp0TransferCompleteEvent(ep_num);
    return;
  }

  UserEndpoint* const uep = get_user_endpoint(ep_num);
  ZX_DEBUG_ASSERT(uep != nullptr);
  if (!uep->server->current_req.has_value()) {
    FDF_LOG(ERROR, "no usb request found to complete!");
    return;
  }
  dwc3_trb_t trb = uep->fifo.Read();

  if (trb.control & TRB_HWO) {
    FDF_LOG(ERROR, "TRB_HWO still set in dwc3_ep_xfer_complete %d", uep->ep.ep_num);
    return;
  }
  uep->server->RequestComplete(
      ZX_OK,
      std::get<usb::FidlRequest>(*uep->server->current_req)->data()->at(0).size().value() -
          TRB_BUFSIZ(trb.status),
      std::move(*uep->server->current_req));
  uep->server->current_req.reset();
  uep->fifo.AdvanceRead();
}

void Dwc3::HandleEpTransferNotReadyEvent(uint8_t ep_num, uint32_t stage) {
  if (is_ep0_num(ep_num)) {
    HandleEp0TransferNotReadyEvent(ep_num, stage);
    return;
  }

  UserEndpoint* const uep = get_user_endpoint(ep_num);
  ZX_DEBUG_ASSERT(uep != nullptr);
  uep->ep.got_not_ready = true;
  UserEpQueueNext(*uep);
}

void Dwc3::HandleEpTransferStartedEvent(uint8_t ep_num, uint32_t rsrc_id) {
  if (is_ep0_num(ep_num)) {
    ((ep_num == kEp0Out) ? ep0_.out : ep0_.in).rsrc_id = rsrc_id;
  } else {
    UserEndpoint* const uep = get_user_endpoint(ep_num);
    ZX_DEBUG_ASSERT(uep != nullptr);
    uep->ep.rsrc_id = rsrc_id;
  }
}

}  // namespace dwc3
