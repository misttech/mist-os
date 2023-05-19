// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BLOCK_DRIVERS_UFS_REQUEST_PROCESSOR_H_
#define SRC_DEVICES_BLOCK_DRIVERS_UFS_REQUEST_PROCESSOR_H_

#include <fuchsia/hardware/block/driver/cpp/banjo.h>
#include <lib/device-protocol/pci.h>
#include <lib/zircon-internal/thread_annotations.h>

#include <ddktl/device.h>

#include "request_list.h"

namespace ufs {

constexpr uint32_t kCommandTimeoutMs = 10000;

class Ufs;

class RequestProcessor {
 public:
  explicit RequestProcessor(RequestList request_list, Ufs &ufs, zx::unowned_bti bti,
                            fdf::MmioBuffer &mmio, uint32_t slot_count)
      : request_list_(std::move(request_list)),
        controller_(ufs),
        register_(mmio),
        bti_(std::move(bti)) {}
  virtual ~RequestProcessor() = default;

  // Write the address of the list to the list base address register and set the run-stop register.
  virtual zx::result<> Init() = 0;
  // Get the number of the free slot and mark it as |SlotState::kReserved|.
  virtual zx::result<uint8_t> ReserveSlot() = 0;

  // Ring the door bell and wait for completion.
  virtual zx::result<> SendRequest(uint8_t slot, bool sync) = 0;
  // Check all slots to process completed requests. This function returns the number of completed
  // requests. This function is called by the ISR.
  virtual uint32_t RequestCompletion() = 0;

  RequestList &GetRequestList() { return request_list_; }

  // For testing
  void SetTimeoutMsec(uint32_t time) { timeout_msec_ = time; }
  uint32_t GetTimeoutMsec() const { return timeout_msec_; }

 protected:
  // TODO(fxbug.dev/124835): It should be changed to slot lock, which is a fine grained lock.
  std::mutex request_list_lock_;
  RequestList request_list_ TA_GUARDED(request_list_lock_);

  Ufs &controller_;
  fdf::MmioBuffer &register_;

  uint32_t timeout_msec_ = kCommandTimeoutMs;

 private:
  zx::unowned_bti bti_;
};

}  // namespace ufs

#endif  // SRC_DEVICES_BLOCK_DRIVERS_UFS_REQUEST_PROCESSOR_H_
