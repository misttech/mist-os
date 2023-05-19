// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BLOCK_DRIVERS_UFS_REQUEST_LIST_H_
#define SRC_DEVICES_BLOCK_DRIVERS_UFS_REQUEST_LIST_H_

#include <lib/ddk/io-buffer.h>
#include <lib/zx/bti.h>
#include <lib/zx/result.h>

#include <cstdint>
#include <vector>

#include <safemath/safe_conversions.h>

#include "upiu/scsi_commands.h"

namespace ufs {

// UFS 3.1 only supports 32 inflight requests.
constexpr uint8_t kMaxRequestListSize = 32;
// The size of the transfer request and the transfer response is less than 1024 bytes. The PRDT
// region limits the number of scatter gathers to 128, using a total of 2048 bytes. Therefore, only
// one page size is allocated for the UTP command descriptor.
constexpr size_t kUtpCommandDescriptorSize = 4096;

enum class SlotState {
  kFree = 0,
  kReserved,
  kScheduled,
};

struct RequestSlot {
  SlotState state = SlotState::kFree;
  // Only populated for SCSI commands, else nullptr.
  std::unique_ptr<scsi_xfer> xfer;
  ddk::IoBuffer command_descriptor_io;
  sync_completion_t complete;
};

// Implements the UTP 'transfer/task management' request list.
class RequestList {
 public:
  static zx::result<RequestList> Create(zx::unowned_bti bti, size_t entry_size,
                                        uint8_t entry_count);

  // Get 'transfer/task management' request descriptor's physical address
  template <typename T>
  zx_paddr_t GetRequestDescriptorPhysicalAddress(uint8_t slot) const {
    return io_buffer_.phys() + sizeof(T) * slot;
  }
  // Get 'transfer/task management' request descriptor's virtual address
  template <typename T>
  T *GetRequestDescriptor(uint8_t slot) const {
    return static_cast<T *>(io_buffer_.virt()) + slot;
  }

  RequestSlot &GetSlot(uint8_t entry_num) {
    ZX_ASSERT_MSG(entry_num < request_slots_.size(), "Invalid entry_num");
    return request_slots_[entry_num];
  }
  uint8_t GetSlotCount() const { return safemath::checked_cast<uint8_t>(request_slots_.size()); }

  template <typename T = void>
  T *GetDescriptorBuffer(uint8_t entry_num, uint16_t offset = 0) {
    ZX_ASSERT_MSG(entry_num < request_slots_.size(), "Invalid entry_num");
    return reinterpret_cast<T *>(
        reinterpret_cast<uint8_t *>(request_slots_[entry_num].command_descriptor_io.virt()) +
        offset);
  }

  size_t GetDescriptorBufferSize(uint8_t entry_num) {
    ZX_ASSERT_MSG(entry_num < request_slots_.size(), "Invalid entry_num");
    return request_slots_[entry_num].command_descriptor_io.size();
  }

 private:
  zx::result<> Init(zx::unowned_bti bti, size_t entry_size, uint8_t entry_count);
  zx::result<> IoBufferInit(zx::unowned_bti &bti, ddk::IoBuffer &io, size_t size);

  ddk::IoBuffer io_buffer_;

  // Information about the requests that exist in the request list.
  std::vector<RequestSlot> request_slots_;
};

}  // namespace ufs

#endif  // SRC_DEVICES_BLOCK_DRIVERS_UFS_REQUEST_LIST_H_
