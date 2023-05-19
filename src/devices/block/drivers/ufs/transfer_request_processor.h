// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BLOCK_DRIVERS_UFS_TRANSFER_REQUEST_PROCESSOR_H_
#define SRC_DEVICES_BLOCK_DRIVERS_UFS_TRANSFER_REQUEST_PROCESSOR_H_

#include "request_processor.h"

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

  zx::result<> SendRequest(uint8_t slot, bool sync) override;
  uint32_t RequestCompletion() override;

  template <typename T>
  zx::result<T *> SendUpiu(RequestUpiu &request) {
    zx::result<void *> response;
    if (response = SendUpiu(request); response.is_error()) {
      return response.take_error();
    }
    T *response_upiu = static_cast<T *>(response.value());

    // Check response.
    if (response_upiu->GetHeader().response != UpiuHeaderResponse::kTargetSuccess) {
      zxlogf(ERROR, "Failed to get response: response=%x", response_upiu->GetHeader().response);
      return zx::error(ZX_ERR_BAD_STATE);
    }
    return zx::ok(response_upiu);
  }
  // TODO(fxbug.dev/124835): |SendUpiu()| and |SendScsiUpiu()| have many of the same behaviours and
  // should be combined into a single method.
  zx::result<ResponseUpiu> SendScsiUpiu(std::unique_ptr<struct scsi_xfer> xfer, uint8_t slot);

  // Create a scsi transfer and add it to the transfer list. The added transfer is processed by the
  // scsi thread. If |event| is nullptr, then the SCSI command is executed synchronously.
  zx::result<> QueueScsiCommand(std::unique_ptr<ScsiCommandUpiu> upiu, uint8_t lun, void *buffer,
                                const zx_paddr_t *buffer_phys, void *cmd_data,
                                sync_completion_t *event);

 private:
  friend class UfsTest;

  zx::result<void *> SendUpiu(RequestUpiu &request);
  // Fill in the transfer request descriptor fields and call the |SendRequest()| method.
  zx::result<> SendCommand(uint8_t slot, TransferRequestDescriptorDataDirection data_dir,
                           uint16_t response_offset, uint16_t response_length, uint16_t prdt_offset,
                           uint32_t prdt_length, bool sync);
  zx::result<> GetResponseStatus(TransferRequestDescriptor *descriptor, ResponseUpiu *response,
                                 uint8_t transaction_type);

  void ScsiCompletion(uint8_t slot_num, RequestSlot &request_slot,
                      TransferRequestDescriptor *descriptor) TA_REQ(request_list_lock_);

  uint32_t slot_mask_;
};

}  // namespace ufs

#endif  // SRC_DEVICES_BLOCK_DRIVERS_UFS_TRANSFER_REQUEST_PROCESSOR_H_
