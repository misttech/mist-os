// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BLOCK_DRIVERS_UFS_TRANSFER_REQUEST_PROCESSOR_H_
#define SRC_DEVICES_BLOCK_DRIVERS_UFS_TRANSFER_REQUEST_PROCESSOR_H_

#include <lib/trace/event.h>

#include "request_processor.h"
#include "src/devices/block/drivers/ufs/registers.h"
#include "src/devices/block/drivers/ufs/transfer_request_descriptor.h"
#include "src/devices/block/drivers/ufs/upiu/scsi_commands.h"

namespace ufs {

constexpr uint8_t kMaxTransferRequestListSize = kMaxRequestListSize;
// Currently, the UFS driver has two threads for submitting commands. One is the ufs driver thread
// that submits the admin command when the driver is initialized, and the other is the I/O thread
// that submits the requested I/O command from the block server.
// These two threads should hold a lock to access the shared resource RequestList slot. However, if
// we separate the Admin slot and the I/O slot, we do not need to lock the slot. Therefore, slots 0
// ~ 30 are used by the I/O thread, and slot 31 is used for the Admin command.
constexpr uint8_t kAdminCommandSlotCount = 1;
constexpr uint8_t kAdminCommandSlotNumber = kMaxTransferRequestListSize - kAdminCommandSlotCount;

// Owns and processes the UTP transfer request list.
class TransferRequestProcessor : public RequestProcessor {
 public:
  static zx::result<std::unique_ptr<TransferRequestProcessor>> Create(Ufs &ufs, zx::unowned_bti bti,
                                                                      const fdf::MmioView mmio,
                                                                      uint8_t entry_count) {
    if (entry_count > kMaxTransferRequestListSize) {
      FDF_LOG(ERROR, "Request list size exceeded the maximum size of %d.",
              kMaxTransferRequestListSize);
      return zx::error(ZX_ERR_INVALID_ARGS);
    }

    return RequestProcessor::Create<TransferRequestProcessor, TransferRequestDescriptor>(
        ufs, std::move(bti), mmio, entry_count);
  }
  explicit TransferRequestProcessor(RequestList request_list, Ufs &ufs, zx::unowned_bti bti,
                                    const fdf::MmioView mmio, uint32_t slot_count)
      : RequestProcessor(std::move(request_list), ufs, std::move(bti), mmio, slot_count) {}
  ~TransferRequestProcessor() override = default;

  zx::result<> Init() override;
  // Allocate a slot to submit an Admin command. Use slot 31 to avoid conflicts with I/O commands.
  zx::result<uint8_t> ReserveAdminSlot() TA_REQ(admin_slot_lock_);

  uint32_t AdminRequestCompletion();
  uint32_t IoRequestCompletion() override;

  zx::result<std::unique_ptr<ResponseUpiu>> SendScsiUpiuUsingSlot(
      ScsiCommandUpiu &request, uint8_t lun, uint8_t slot, std::optional<zx::unowned_vmo> data_vmo,
      IoCommand *io_cmd, bool is_sync);

  // |SendScsiUpiu| allocates a slot for SCSI command UPIU and calls SendScsiUpiuUsingSlot.
  // If it is an admin command, the |io_cmd| is nullptr.
  zx::result<std::unique_ptr<ResponseUpiu>> SendScsiUpiu(
      ScsiCommandUpiu &request, uint8_t lun, std::optional<zx::unowned_vmo> data = std::nullopt,
      IoCommand *io_cmd = nullptr);

  // This function is a wrapper function that sends a query request UPIU.
  zx::result<std::unique_ptr<QueryResponseUpiu>> SendQueryRequestUpiu(QueryRequestUpiu &request);

  // |SendRequestUpiu| allocates a slot for request UPIU and calls SendRequestUsingSlot.
  // This function is only ever used for admin commands.
  template <class RequestType, class ResponseType>
  zx::result<std::unique_ptr<ResponseType>> SendRequestUpiu(RequestType &request, uint8_t lun = 0) {
    std::lock_guard<std::mutex> lock(admin_slot_lock_);
    zx::result<uint8_t> slot = ReserveAdminSlot();
    if (slot.is_error()) {
      return zx::error(ZX_ERR_NO_RESOURCES);
    }

    zx::result<void *> response;
    if (response = SendRequestUsingSlot<RequestType>(request, lun, slot.value(), std::nullopt,
                                                     nullptr, /*is_sync*/ true);
        response.is_error()) {
      return response.take_error();
    }
    auto response_upiu = std::make_unique<ResponseType>(response.value());

    return zx::ok(std::move(response_upiu));
  }

  template <class RequestType>
  std::tuple<uint16_t, uint32_t> PreparePrdt(RequestType &request, uint8_t lun, uint8_t slot,
                                             const std::vector<zx_paddr_t> &buffer_phys,
                                             uint16_t response_offset, uint16_t response_length) {
    return {0, 0};
  }

  template <>
  std::tuple<uint16_t, uint32_t> PreparePrdt<ScsiCommandUpiu>(
      ScsiCommandUpiu &request, uint8_t lun, uint8_t slot,
      const std::vector<zx_paddr_t> &buffer_phys, uint16_t response_offset,
      uint16_t response_length);

  template <class RequestType>
  zx::result<void *> SendRequestUsingSlot(RequestType &request, uint8_t lun, uint8_t slot,
                                          std::optional<zx::unowned_vmo> data_vmo,
                                          IoCommand *io_cmd, bool is_sync);

 private:
  friend class UfsTest;

  zx::result<> FillDescriptorAndSendRequest(uint8_t slot, DataDirection data_dir,
                                            uint16_t response_offset, uint16_t response_length,
                                            uint16_t prdt_offset, uint32_t prdt_entry_count);
  zx::result<> GetResponseStatus(TransferRequestDescriptor *descriptor,
                                 AbstractResponseUpiu &response,
                                 UpiuTransactionCodes transaction_code);

  zx::result<> ScsiCompletion(uint8_t slot_num, RequestSlot &request_slot,
                              TransferRequestDescriptor *descriptor);

  zx::result<uint8_t> GetAdminCommandSlotNumber() override {
    return zx::ok(kAdminCommandSlotNumber);
  }

  void SetDoorBellRegister(uint8_t slot_num) override {
    UtrListDoorBellReg::Get().FromValue(1 << slot_num).WriteTo(&register_);
  }
  bool ProcessSlotCompletion(uint8_t slot_num);

  // TODO(b/42075643): Background Operation uses the admin slot, causing a race condition for admin
  // commands running on the main thread. To fix this, per-slot locking is required, but I added
  // admin_slot_lock_ as a temporary solution.
  std::mutex admin_slot_lock_;
};

}  // namespace ufs

#endif  // SRC_DEVICES_BLOCK_DRIVERS_UFS_TRANSFER_REQUEST_PROCESSOR_H_
