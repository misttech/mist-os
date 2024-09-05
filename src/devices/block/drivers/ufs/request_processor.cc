// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "request_processor.h"

#include "src/devices/block/drivers/ufs/ufs.h"

namespace ufs {

template <typename Processor, typename Descriptor>
zx::result<std::unique_ptr<Processor>> RequestProcessor::Create(Ufs &ufs, zx::unowned_bti bti,
                                                                const fdf::MmioView mmio,
                                                                uint8_t entry_count) {
  zx::result<RequestList> request_list =
      RequestList::Create(bti->borrow(), sizeof(Descriptor), entry_count);
  if (request_list.is_error()) {
    return request_list.take_error();
  }

  fbl::AllocChecker ac;
  auto request_processor = fbl::make_unique_checked<Processor>(
      &ac, std::move(request_list.value()), ufs, std::move(bti), mmio, entry_count);
  if (!ac.check()) {
    FDF_LOG(ERROR, "Failed to allocate request processor.");
    return zx::error(ZX_ERR_NO_MEMORY);
  }
  return zx::ok(std::move(request_processor));
}

template zx::result<std::unique_ptr<TransferRequestProcessor>>
RequestProcessor::Create<TransferRequestProcessor, TransferRequestDescriptor>(
    Ufs &ufs, zx::unowned_bti bti, const fdf::MmioView mmio, uint8_t entry_count);

template zx::result<std::unique_ptr<TaskManagementRequestProcessor>>
RequestProcessor::Create<TaskManagementRequestProcessor, TaskManagementRequestDescriptor>(
    Ufs &ufs, zx::unowned_bti bti, const fdf::MmioView mmio, uint8_t entry_count);

zx::result<uint8_t> RequestProcessor::ReserveSlot() {
  for (uint8_t slot_num = 0; slot_num < request_list_.GetSlotCount(); ++slot_num) {
    // Skip admin slot
    zx::result<uint8_t> admin_slot_num = GetAdminCommandSlotNumber();
    if (admin_slot_num.is_ok() && slot_num == admin_slot_num.value()) {
      continue;
    }
    RequestSlot &slot = request_list_.GetSlot(slot_num);
    if (slot.state == SlotState::kFree) {
      slot.state = SlotState::kReserved;
      return zx::ok(slot_num);
    }
  }
  FDF_LOG(DEBUG, "Failed to reserve a request slot");
  return zx::error(ZX_ERR_NO_RESOURCES);
}

zx::result<> RequestProcessor::ClearSlot(RequestSlot &request_slot) {
  if (request_slot.pmt.is_valid()) {
    if (zx_status_t status = request_slot.pmt.unpin(); status != ZX_OK) {
      FDF_LOG(ERROR, "Failed to unpin IO buffer: %s", zx_status_get_string(status));
      return zx::error(status);
    }
  }

  request_slot.state = SlotState::kFree;
  request_slot.io_cmd = nullptr;
  request_slot.is_scsi_command = false;
  request_slot.is_sync = false;
  request_slot.result = ZX_OK;

  return zx::ok();
}

zx::result<> RequestProcessor::RingRequestDoorbell(uint8_t slot_num) {
  RequestSlot &request_slot = request_list_.GetSlot(slot_num);
  sync_completion_t *complete = &request_slot.complete;
  sync_completion_reset(complete);
  ZX_DEBUG_ASSERT(request_slot.state == SlotState::kReserved);
  request_slot.state = SlotState::kScheduled;

  // TODO(https://fxbug.dev/42075643): Set the UtrInterruptAggregationControlReg.

  SetDoorBellRegister(slot_num);

  return zx::ok();
}

}  // namespace ufs
