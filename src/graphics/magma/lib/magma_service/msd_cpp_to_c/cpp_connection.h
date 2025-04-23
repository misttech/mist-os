// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_MAGMA_LIB_MAGMA_SERVICE_MSD_CPP_TO_C_CPP_CONNECTION_H_
#define SRC_GRAPHICS_MAGMA_LIB_MAGMA_SERVICE_MSD_CPP_TO_C_CPP_CONNECTION_H_

#include <lib/magma_service/msd.h>
#include <lib/magma_service/msd_c.h>

namespace msd {
class CppConnection : public msd::Connection {
 public:
  explicit CppConnection(struct MsdConnection* connection, uint64_t client_id);

  ~CppConnection() override;

  magma_status_t MapBuffer(msd::Buffer& buffer, uint64_t gpu_va, uint64_t offset, uint64_t length,
                           uint64_t flags) override;

  void ReleaseBuffer(msd::Buffer& buffer, bool shutting_down) override;

  std::unique_ptr<msd::Context> CreateContext() override;

 private:
  struct MsdConnection* connection_;
};
}  // namespace msd

#endif  // SRC_GRAPHICS_MAGMA_LIB_MAGMA_SERVICE_MSD_CPP_TO_C_CPP_CONNECTION_H_
