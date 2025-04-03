// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "cpp_device.h"

#include <lib/magma/util/macros.h>

#include "cpp_connection.h"
#include "sdk/lib/magma_common/include/lib/magma/magma_common_defs.h"

namespace msd {
CppDevice::~CppDevice() { msd_device_release(device_); }

magma_status_t CppDevice::Query(uint64_t id, zx::vmo* result_buffer_out, uint64_t* result_out) {
  return msd_device_query(device_, id,
                          result_buffer_out ? result_buffer_out->reset_and_get_address() : nullptr,
                          result_out);
}

magma_status_t CppDevice::GetIcdList(std::vector<MsdIcdInfo>* icd_info_out) {
  ZX_DEBUG_ASSERT(icd_info_out);

  std::vector<MsdIcdInfo> icd_info = {MsdIcdInfo{
      .component_url = "fuchsia-pkg://fuchsia.com/vulkan_freedreno#meta/vulkan.cm",
      .support_flags = ICD_SUPPORT_FLAG_VULKAN,
  }};

  *icd_info_out = std::move(icd_info);

  return MAGMA_STATUS_OK;
}

std::unique_ptr<Connection> CppDevice::Open(msd_client_id_t client_id) {
  struct MsdConnection* msd_connection = msd_device_create_connection(device_, client_id);
  if (!msd_connection)
    return MAGMA_DRETP(nullptr, "msd_device_create_connection failed");

  return std::make_unique<CppConnection>(msd_connection, client_id);
}
}  // namespace msd
