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
      std::optional<std::unique_ptr<scsi_xfer>> xfer = std::nullopt);

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
