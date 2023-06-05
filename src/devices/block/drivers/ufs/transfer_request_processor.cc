// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "transfer_request_processor.h"

#include <safemath/checked_math.h>
#include <safemath/safe_conversions.h>

#include "ufs.h"

namespace ufs {

namespace {
void FillPrdt(PhysicalRegionDescriptionTableEntry *prdt,
              std::array<zx_paddr_t, 2> buffer_physical_addresses, uint32_t prdt_count,
              uint32_t data_length) {
  ZX_ASSERT(prdt_count <= 2);

  for (uint32_t i = 0; i < prdt_count; ++i) {
    ZX_ASSERT(buffer_physical_addresses[i] != 0);
    uint32_t byte_count = data_length < kPrdtEntryDataLength ? data_length : kPrdtEntryDataLength;
    prdt->set_data_base_address(static_cast<uint32_t>(buffer_physical_addresses[i] & 0xffffffff));
    prdt->set_data_base_address_upper(static_cast<uint32_t>(buffer_physical_addresses[i] >> 32));
    prdt->set_data_byte_count(byte_count - 1);

    ++prdt;
    data_length -= byte_count;
  }
  ZX_DEBUG_ASSERT(data_length == 0);
}
}  // namespace

zx::result<std::unique_ptr<TransferRequestProcessor>> TransferRequestProcessor::Create(
    Ufs &ufs, zx::unowned_bti bti, fdf::MmioBuffer &mmio, uint8_t entry_count) {
  if (entry_count > kMaxTransferRequestListSize) {
    zxlogf(ERROR, "Request list size exceeded the maximum size of %d.",
           kMaxTransferRequestListSize);
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  zx::result<RequestList> request_list =
      RequestList::Create(bti->borrow(), sizeof(TransferRequestDescriptor), entry_count);
  if (request_list.is_error()) {
    return request_list.take_error();
  }

  fbl::AllocChecker ac;
  auto request_processor = fbl::make_unique_checked<TransferRequestProcessor>(
      &ac, std::move(request_list.value()), ufs, std::move(bti), mmio, entry_count);
  if (!ac.check()) {
    zxlogf(ERROR, "Failed to allocate transfer request processor.");
    return zx::error(ZX_ERR_NO_MEMORY);
  }
  return zx::ok(std::move(request_processor));
}

zx::result<> TransferRequestProcessor::Init() {
  std::lock_guard lock(request_list_lock_);
  zx_paddr_t paddr =
      request_list_.GetRequestDescriptorPhysicalAddress<TransferRequestDescriptor>(0);
  UtrListBaseAddressReg::Get().FromValue(paddr & 0xffffffff).WriteTo(&register_);
  UtrListBaseAddressUpperReg::Get().FromValue(paddr >> 32).WriteTo(&register_);

  slot_mask_ = static_cast<uint32_t>(1UL << request_list_.GetSlotCount()) - 1;

  // Start Utp Transfer Request list.
  UtrListRunStopReg::Get().FromValue(0).set_value(true).WriteTo(&register_);

  return zx::ok();
}

zx::result<uint8_t> TransferRequestProcessor::ReserveSlot() {
  std::lock_guard lock(request_list_lock_);
  for (uint8_t slot_num = 0; slot_num < request_list_.GetSlotCount(); ++slot_num) {
    RequestSlot &slot = request_list_.GetSlot(slot_num);
    if (slot.state == SlotState::kFree) {
      slot.state = SlotState::kReserved;
      if (slot.xfer) {
        // Release old SCSI transfer.
        slot.xfer = nullptr;
      }
      return zx::ok(slot_num);
    }
  }

  return zx::error(ZX_ERR_NO_RESOURCES);
}

zx::result<void *> TransferRequestProcessor::SendUpiu(RequestUpiu &request) {
  zx::result<uint8_t> slot = ReserveSlot();
  if (slot.is_error()) {
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  const uint16_t response_offset = request.GetResponseOffset();
  const uint16_t response_length = request.GetResponseLength();

  const size_t total_length = static_cast<size_t>(response_offset) + response_length;

  // Copy request and prepare response.
  void *response;
  {
    std::lock_guard lock(request_list_lock_);
    ZX_DEBUG_ASSERT_MSG(total_length <= request_list_.GetDescriptorBufferSize(slot.value()),
                        "Invalid UPIU size");
    memcpy(request_list_.GetDescriptorBuffer(slot.value()), request.GetData(), response_offset);
    memset(request_list_.GetDescriptorBuffer<uint8_t>(slot.value()) + response_offset, 0,
           response_length);
    response = request_list_.GetDescriptorBuffer(slot.value(), response_offset);
  }

  // Record the slot number to |task_tag| for debugging.
  request.GetHeader().task_tag = slot.value();

  if (zx::result<> result = SendCommand(slot.value(), request.GetDataDirection(), response_offset,
                                        response_length, 0, 0, /*sync=*/true);
      result.is_error()) {
    zxlogf(ERROR, "Failed to send UPIU: %s", result.status_string());
    return result.take_error();
  }

  return zx::ok(response);
}

zx::result<ResponseUpiu> TransferRequestProcessor::SendScsiUpiu(std::unique_ptr<scsi_xfer> xfer,
                                                                uint8_t slot, bool sync) {
  ScsiCommandUpiu *request = xfer->upiu.get();

  const uint16_t response_offset = request->GetResponseOffset();
  const uint16_t response_length = request->GetResponseLength();

  // Copy request and prepare response.
  ResponseUpiu *response;
  {
    std::lock_guard lock(request_list_lock_);
    memcpy(request_list_.GetDescriptorBuffer(slot), request->GetData(), response_offset);
    memset(request_list_.GetDescriptorBuffer<uint8_t>(slot) + response_offset, 0, response_length);
    response = request_list_.GetDescriptorBuffer<ResponseUpiu>(slot, response_offset);
  }

  // Prepare PRDT(physical region description table).
  const uint32_t data_transfer_length = std::min(request->GetTransferBytes(), kMaxPrdtDataLength);
  const uint32_t prdt_entry_count =
      fbl::round_up(data_transfer_length, kPrdtEntryDataLength) / kPrdtEntryDataLength;
  ZX_DEBUG_ASSERT(prdt_entry_count <= kMaxPrdtNum);

  const uint16_t prdt_offset = response_offset + response_length;
  const uint32_t prdt_length = prdt_entry_count * sizeof(PhysicalRegionDescriptionTableEntry);

  const size_t total_length = static_cast<size_t>(prdt_offset) + prdt_length;

  PhysicalRegionDescriptionTableEntry *prdt;
  {
    std::lock_guard lock(request_list_lock_);
    ZX_DEBUG_ASSERT_MSG(total_length <= request_list_.GetDescriptorBufferSize(slot),
                        "Invalid UPIU size");
    prdt =
        request_list_.GetDescriptorBuffer<PhysicalRegionDescriptionTableEntry>(slot, prdt_offset);
    memset(prdt, 0, prdt_length);
  }
  FillPrdt(prdt, xfer->buffer_phys, prdt_entry_count, data_transfer_length);

  request->GetHeader().lun = xfer->lun;
  request->GetHeader().task_tag = slot;  // Record the slot number to |task_tag| for debugging.
  request->SetExpectedDataTransferLength(data_transfer_length);

  // TODO(fxbug.dev/124835): Enable unmmap and write buffer command. Umap and writebuffer must set
  // the xfer->count value differently.
  // TODO(fxbug.dev/124835): Support large size transfer.

  if (zxlog_level_enabled(TRACE)) {
    std::lock_guard lock(request_list_lock_);
    zxlogf(TRACE, "1. SCSI: Command Descriptor = 0x%lx",
           request_list_.GetRequestDescriptorPhysicalAddress<TransferRequestDescriptor>(slot));
    zxlogf(TRACE, "2. SCSI: Command UPIU = 0x%lx",
           request_list_.GetSlot(slot).command_descriptor_io.phys());
    zxlogf(TRACE, "3. SCSI: Response UPIU = 0x%lx",
           request_list_.GetSlot(slot).command_descriptor_io.phys() + response_offset);
    zxlogf(TRACE, "4. SCSI: PRDT = 0x%lx",
           request_list_.GetSlot(slot).command_descriptor_io.phys() + response_offset +
               response_length);
    zxlogf(TRACE, "5. SCSI: Data Buffer = 0x%lx, 0x%lx", xfer->buffer_phys[0],
           xfer->buffer_phys[1]);
    zxlogf(TRACE, "6. SCSI: PRDT prdt_offset = %hu, prdt_length = %hu, prdt_entry_count = %d",
           prdt_offset, prdt_length, prdt_entry_count);
  }

  {
    std::lock_guard lock(request_list_lock_);
    RequestSlot &request_slot = request_list_.GetSlot(slot);
    request_slot.xfer = std::move(xfer);
  }

  if (zx::result<> result = SendCommand(slot, request->GetDataDirection(), response_offset,
                                        response_length, prdt_offset, prdt_length, sync);
      result.is_error()) {
    ScsiSenseData *sense_data = reinterpret_cast<ScsiSenseData *>(response->GetSenseData());
    zxlogf(ERROR, "Failed to send scsi command upiu, response code 0x%x, sense key 0x%x",
           sense_data->resp_code, sense_data->sense_key);
    return result.take_error();
  }

  return zx::ok(*response);
}

void TransferRequestProcessor::ScsiCompletion(uint8_t slot_num, RequestSlot &request_slot,
                                              TransferRequestDescriptor *descriptor) {
  // TODO(fxbug.dev/124835): Support large size transfer.
  ResponseUpiu *response =
      request_list_.GetDescriptorBuffer<ResponseUpiu>(slot_num, CommandUpiu::GetDataSize());

  // TODO(fxbug.dev/124835): Need to check if response.header.trans_code() is a kCommnad.
  request_slot.xfer->status =
      GetResponseStatus(descriptor, response, UpiuTransactionCodes::kCommand).status_value();

  sync_completion_signal(request_slot.xfer->done);
}

zx::result<> TransferRequestProcessor::SendRequest(uint8_t slot_num, bool sync) {
  ZX_DEBUG_ASSERT_MSG(UtrListRunStopReg::Get().ReadFrom(&register_).value(),
                      "Transfer request list is not running");

  if (zx::result<> result = controller_.Notify(NotifyEvent::kSetupTransferRequestList, slot_num);
      result.is_error()) {
    return result.take_error();
  }

  sync_completion_t *complete;
  {
    std::lock_guard lock(request_list_lock_);
    RequestSlot &request_slot = request_list_.GetSlot(slot_num);
    complete = &request_slot.complete;
    sync_completion_reset(complete);
    ZX_ASSERT(request_slot.state == SlotState::kReserved);
    request_slot.state = SlotState::kScheduled;

    UtrListDoorBellReg::Get().FromValue(1 << slot_num).WriteTo(&register_);
  }

  if (!sync) {
    return zx::ok();
  }

  // Wait for completion.
  if (zx_status_t status = sync_completion_wait(complete, ZX_MSEC(GetTimeoutMsec()));
      status != ZX_OK) {
    return zx::error(status);
  }
  return zx::ok();
}

uint32_t TransferRequestProcessor::RequestCompletion() {
  uint32_t completion_count = 0;

  // Search for all pending slots and signed the ones already done.
  {
    // TODO(fxbug.dev/124835): Need to make sure it can be done without a lock
    std::lock_guard lock(request_list_lock_);
    for (uint8_t slot_num = 0; slot_num < request_list_.GetSlotCount(); ++slot_num) {
      RequestSlot &request_slot = request_list_.GetSlot(slot_num);
      if (request_slot.state == SlotState::kScheduled) {
        if (!(UtrListDoorBellReg::Get().ReadFrom(&register_).door_bell() & (1 << slot_num))) {
          // Is SCSI command
          if (request_slot.xfer) {
            auto descriptor =
                request_list_.GetRequestDescriptor<TransferRequestDescriptor>(slot_num);
            ScsiCompletion(slot_num, request_slot, descriptor);
          }

          UtrListCompletionNotificationReg::Get()
              .FromValue(0)
              .set_notification(1 << slot_num)
              .WriteTo(&register_);

          request_slot.state = SlotState::kFree;
          sync_completion_signal(&request_slot.complete);
          ++completion_count;
        }
      }
    }
  }

  sync_completion_signal(&controller_.GetScsiEvent());

  return completion_count;
}

zx::result<> TransferRequestProcessor::SendCommand(
    uint8_t slot, const TransferRequestDescriptorDataDirection data_dir,
    const uint16_t response_offset, const uint16_t response_length, const uint16_t prdt_offset,
    const uint32_t prdt_length, bool sync) {
  TransferRequestDescriptor *descriptor;
  zx_paddr_t paddr = 0;
  {
    std::lock_guard lock(request_list_lock_);
    descriptor = request_list_.GetRequestDescriptor<TransferRequestDescriptor>(slot);
    paddr = request_list_.GetSlot(slot).command_descriptor_io.phys();
  }

  // Fill up UTP Transfer Request Descriptor.
  memset(descriptor, 0, sizeof(TransferRequestDescriptor));
  descriptor->set_interrupt(true);
  descriptor->set_data_direction(data_dir);
  descriptor->set_command_type(kCommandTypeUfsStorage);
  descriptor->set_overall_command_status(OverallCommandStatus::kSuccess);
  descriptor->set_utp_command_descriptor_base_address(static_cast<uint32_t>(paddr & 0xffffffff));
  descriptor->set_utp_command_descriptor_base_address_upper(static_cast<uint32_t>(paddr >> 32));

  constexpr uint16_t kDwordSize = 4;
  descriptor->set_response_upiu_offset(response_offset / kDwordSize);
  descriptor->set_response_upiu_length(response_length / kDwordSize);
  descriptor->set_prdt_offset(prdt_offset / kDwordSize);
  descriptor->set_prdt_length(prdt_length / kDwordSize);

  if (zx::result<> result = SendRequest(slot, sync); result.is_error()) {
    zxlogf(ERROR, "Failed to send cmd %s", result.status_string());
    return result.take_error();
  }

  if (!sync) {
    return zx::ok();
  }

  std::lock_guard lock(request_list_lock_);
  auto header = request_list_.GetDescriptorBuffer<UpiuHeader>(slot);
  auto response = request_list_.GetDescriptorBuffer<ResponseUpiu>(slot, response_offset);
  return GetResponseStatus(descriptor, response, header->trans_code());
}

zx::result<> TransferRequestProcessor::GetResponseStatus(TransferRequestDescriptor *descriptor,
                                                         ResponseUpiu *response,
                                                         uint8_t transaction_type) {
  uint8_t status = response->GetHeader().status;
  uint8_t header_response = response->GetHeader().response;

  // TODO(fxbug.dev/124835): Needs refactoring.
  if (transaction_type == UpiuTransactionCodes::kCommand &&
      (descriptor->overall_command_status() != OverallCommandStatus::kSuccess ||
       status != ScsiCommandSetStatus::kGood ||
       header_response != UpiuHeaderResponse::kTargetSuccess)) {
    zxlogf(ERROR, "SCSI failure: ocs=0x%x status=0x%x header_response=0x%x",
           descriptor->overall_command_status(), status, header_response);
    ScsiSenseData *sense_data = reinterpret_cast<ScsiSenseData *>(response->GetSenseData());
    zxlogf(ERROR, "SCSI sense data:sense_key=0x%x asc=0x%x ascq=0x%x", sense_data->sense_key,
           sense_data->asc, sense_data->ascq);
  } else if (transaction_type == UpiuTransactionCodes::kQueryRequest &&
             (descriptor->overall_command_status() != OverallCommandStatus::kSuccess ||
              header_response != UpiuHeaderResponse::kTargetSuccess)) {
    zxlogf(ERROR, "Query failure: ocs=0x%x header_response=0x%x",
           descriptor->overall_command_status(), header_response);
  } else if (descriptor->overall_command_status() != OverallCommandStatus::kSuccess) {
    zxlogf(ERROR, "Generic failure: ocs=0x%x", descriptor->overall_command_status());
  } else {
    return zx::ok();
  }

  return zx::error(ZX_ERR_BAD_STATE);
}

}  // namespace ufs
