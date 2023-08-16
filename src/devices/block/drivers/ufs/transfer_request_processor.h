// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BLOCK_DRIVERS_UFS_TRANSFER_REQUEST_PROCESSOR_H_
#define SRC_DEVICES_BLOCK_DRIVERS_UFS_TRANSFER_REQUEST_PROCESSOR_H_

#include <lib/trace/event.h>

#include "request_processor.h"
#include "src/devices/block/drivers/ufs/upiu/scsi_commands.h"

namespace ufs {

constexpr uint8_t kMaxTransferRequestListSize = kMaxRequestListSize;
constexpr uint32_t kMaxPrdtLength = 2048;
constexpr uint32_t kMaxPrdtNum = kMaxPrdtLength / sizeof(PhysicalRegionDescriptionTableEntry);
constexpr uint32_t kPrdtEntryDataLength = 4096;                              // 4KiB
constexpr uint32_t kMaxPrdtDataLength = kMaxPrdtNum * kPrdtEntryDataLength;  // 512KiB

// Owns and processes the UTP transfer request list.
class TransferRequestProcessor : public RequestProcessor {
 public:
  static zx::result<std::unique_ptr<TransferRequestProcessor>> Create(Ufs &ufs, zx::unowned_bti bti,
                                                                      fdf::MmioBuffer &mmio,
                                                                      uint8_t entry_count);
  explicit TransferRequestProcessor(RequestList request_list, Ufs &ufs, zx::unowned_bti bti,
                                    fdf::MmioBuffer &mmio, uint32_t slot_count)
      : RequestProcessor(std::move(request_list), ufs, std::move(bti), mmio, slot_count) {}
  ~TransferRequestProcessor() override = default;

  zx::result<> Init() override;
  zx::result<uint8_t> ReserveSlot() override;

  zx::result<> RingRequestDoorbell(uint8_t slot, bool sync) override;
  uint32_t RequestCompletion() override;

  // |SendRequestUpiu| allocates a slot for request UPIU and calls SendRequestUsingSlot.
  template <class RequestType, class ResponseType>
  zx::result<std::unique_ptr<ResponseType>> SendRequestUpiu(RequestType &request) {
    zx::result<uint8_t> slot = ReserveSlot();
    if (slot.is_error()) {
      return zx::error(ZX_ERR_NO_RESOURCES);
    }

    zx::result<void *> response;
    if (response = SendRequestUsingSlot<RequestType>(request, slot.value()); response.is_error()) {
      return response.take_error();
    }
    auto response_upiu = std::make_unique<ResponseType>(response.value());

    // Check response.
    if (response_upiu->GetHeader().response != UpiuHeaderResponse::kTargetSuccess) {
      zxlogf(ERROR, "Failed to get response: response=%x", response_upiu->GetHeader().response);
      return zx::error(ZX_ERR_BAD_STATE);
    }
    return zx::ok(std::move(response_upiu));
  }

  template <class RequestType>
  std::tuple<uint16_t, uint32_t> PreparePrdt(RequestType &request, uint8_t slot,
                                             const scsi_xfer *xfer, uint16_t response_offset,
                                             uint16_t response_length) {
    return {0, 0};
  }

  template <>
  std::tuple<uint16_t, uint32_t> PreparePrdt<ScsiCommandUpiu>(ScsiCommandUpiu &request,
                                                              uint8_t slot, const scsi_xfer *xfer,
                                                              uint16_t response_offset,
                                                              uint16_t response_length);

  template <class RequestType>
  zx::result<void *> SendRequestUsingSlot(
      RequestType &request, uint8_t slot,
      std::optional<std::unique_ptr<scsi_xfer>> xfer = std::nullopt) {
    const bool is_scsi = std::is_base_of<ScsiCommandUpiu, RequestType>::value;

    const uint16_t response_offset = request.GetResponseOffset();
    const uint16_t response_length = request.GetResponseLength();

    if (is_scsi) {
      ZX_ASSERT(xfer != std::nullopt && xfer.value() != nullptr);
      TRACE_DURATION_BEGIN("ufs", "SendRequestUsingSlot SCSI command", "offset",
                           xfer.value()->start_lba, "length", xfer.value()->block_count);
    }

    // Record the slot number to |task_tag| for debugging.
    request.GetHeader().task_tag = slot;
    auto [prdt_offset, prdt_entry_count] = PreparePrdt<RequestType>(
        request, slot, (is_scsi ? xfer->get() : nullptr), response_offset, response_length);

    // Copy request and prepare response.
    void *response;
    {
      std::lock_guard lock(request_list_lock_);
      RequestSlot &request_slot = request_list_.GetSlot(slot);
      ZX_ASSERT_MSG(request_slot.state == SlotState::kReserved, "Invalid slot state");
      ZX_ASSERT_MSG(request_slot.xfer == nullptr, "Slot already occupied");

      const size_t length = static_cast<size_t>(response_offset) + response_length;
      ZX_DEBUG_ASSERT_MSG(length <= request_list_.GetDescriptorBufferSize(slot),
                          "Invalid UPIU size");

      memcpy(request_list_.GetDescriptorBuffer(slot), request.GetData(), response_offset);
      memset(request_list_.GetDescriptorBuffer<uint8_t>(slot) + response_offset, 0,
             response_length);
      response = request_list_.GetDescriptorBuffer(slot, response_offset);

      if (is_scsi) {
        request_slot.xfer = std::move(xfer.value());
      }
    }

    if (zx::result<> result = FillDescriptorAndSendRequest(
            slot, request.GetDataDirection(), response_offset, response_length, prdt_offset,
            prdt_entry_count, /*sync=*/true);
        result.is_error()) {
      if (is_scsi) {
        auto *sense_data = reinterpret_cast<scsi::FixedFormatSenseDataHeader *>(
            ResponseUpiu(response).GetSenseData());
        zxlogf(ERROR, "Failed to send scsi command upiu, response code 0x%x, sense key 0x%x",
               sense_data->response_code(), sense_data->sense_key());
      } else {
        zxlogf(ERROR, "Failed to send upiu: %s", result.status_string());
      }

      return result.take_error();
    }

    if (is_scsi) {
      TRACE_DURATION_END("ufs", "SendRequestUsingSlot SCSI command");
    }

    return zx::ok(response);
  }

 private:
  friend class UfsTest;

  zx::result<> FillDescriptorAndSendRequest(uint8_t slot,
                                            TransferRequestDescriptorDataDirection data_dir,
                                            uint16_t response_offset, uint16_t response_length,
                                            uint16_t prdt_offset, uint32_t prdt_entry_count,
                                            bool sync);
  zx::result<> GetResponseStatus(TransferRequestDescriptor *descriptor,
                                 AbstractResponseUpiu &response, uint8_t transaction_type);

  void ScsiCompletion(uint8_t slot_num, RequestSlot &request_slot,
                      TransferRequestDescriptor *descriptor) TA_REQ(request_list_lock_);

  uint32_t slot_mask_;
};

}  // namespace ufs

#endif  // SRC_DEVICES_BLOCK_DRIVERS_UFS_TRANSFER_REQUEST_PROCESSOR_H_
