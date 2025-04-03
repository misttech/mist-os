// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_MAGMA_LIB_MAGMA_SERVICE_MSD_CPP_TO_C_CPP_DEVICE_H_
#define SRC_GRAPHICS_MAGMA_LIB_MAGMA_SERVICE_MSD_CPP_TO_C_CPP_DEVICE_H_

#include <lib/magma_service/msd.h>
#include <lib/magma_service/msd_c.h>
#include <lib/zx/vmo.h>

namespace msd {
class CppDevice : public Device {
 public:
  explicit CppDevice(struct MsdDevice* device) : device_(device) {}

  ~CppDevice();

  magma_status_t Query(uint64_t id, zx::vmo* result_buffer_out, uint64_t* result_out) override;

  magma_status_t GetIcdList(std::vector<MsdIcdInfo>* icd_info_out) override;

  std::unique_ptr<Connection> Open(msd_client_id_t client_id) override;

 private:
  struct MsdDevice* device_;
};
}  // namespace msd

#endif  // SRC_GRAPHICS_MAGMA_LIB_MAGMA_SERVICE_MSD_CPP_TO_C_CPP_DEVICE_H_
