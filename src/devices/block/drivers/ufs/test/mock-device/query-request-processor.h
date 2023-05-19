// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BLOCK_DRIVERS_UFS_TEST_MOCK_DEVICE_QUERY_REQUEST_PROCESSOR_H_
#define SRC_DEVICES_BLOCK_DRIVERS_UFS_TEST_MOCK_DEVICE_QUERY_REQUEST_PROCESSOR_H_

#include <lib/mmio-ptr/fake.h>
#include <lib/mmio/mmio-buffer.h>

#include <functional>
#include <vector>

#include "handler.h"
#include "src/devices/block/drivers/ufs/ufs.h"

namespace ufs {
namespace ufs_mock_device {

class UfsMockDevice;

class QueryRequestProcessor {
 public:
  using QueryRequestHandler = std::function<zx_status_t(UfsMockDevice &, QueryRequestUpiu::Data &,
                                                        QueryResponseUpiu::Data &)>;

  QueryRequestProcessor(const QueryRequestProcessor &) = delete;
  QueryRequestProcessor &operator=(const QueryRequestProcessor &) = delete;
  QueryRequestProcessor(const QueryRequestProcessor &&) = delete;
  QueryRequestProcessor &operator=(const QueryRequestProcessor &&) = delete;
  ~QueryRequestProcessor() = default;
  explicit QueryRequestProcessor(UfsMockDevice &mock_device) : mock_device_(mock_device) {}

  zx_status_t HandleQueryRequest(QueryRequestUpiu::Data &req_upiu,
                                 QueryResponseUpiu::Data &rsp_upiu);

  static zx_status_t DefaultReadDescriptorHandler(UfsMockDevice &mock_device,
                                                  QueryRequestUpiu::Data &req_upiu,
                                                  QueryResponseUpiu::Data &rsp_upiu);
  static zx_status_t DefaultReadAttributeHandler(UfsMockDevice &mock_device,
                                                 QueryRequestUpiu::Data &req_upiu,
                                                 QueryResponseUpiu::Data &rsp_upiu);
  static zx_status_t DefaultWriteAttributeHandler(UfsMockDevice &mock_device,
                                                  QueryRequestUpiu::Data &req_upiu,
                                                  QueryResponseUpiu::Data &rsp_upiu);
  static zx_status_t DefaultReadFlagHandler(UfsMockDevice &mock_device,
                                            QueryRequestUpiu::Data &req_upiu,
                                            QueryResponseUpiu::Data &rsp_upiu);
  static zx_status_t DefaultSetFlagHandler(UfsMockDevice &mock_device,
                                           QueryRequestUpiu::Data &req_upiu,
                                           QueryResponseUpiu::Data &rsp_upiu);
  static zx_status_t DefaultToggleFlagHandler(UfsMockDevice &mock_device,
                                              QueryRequestUpiu::Data &req_upiu,
                                              QueryResponseUpiu::Data &rsp_upiu);
  static zx_status_t DefaultClearFlagHandler(UfsMockDevice &mock_device,
                                             QueryRequestUpiu::Data &req_upiu,
                                             QueryResponseUpiu::Data &rsp_upiu);

  DEF_DEFAULT_HANDLER_BEGIN(QueryOpcode, QueryRequestHandler)
  DEF_DEFAULT_HANDLER(QueryOpcode::kReadDescriptor, DefaultReadDescriptorHandler)
  DEF_DEFAULT_HANDLER(QueryOpcode::kReadAttribute, DefaultReadAttributeHandler)
  DEF_DEFAULT_HANDLER(QueryOpcode::kWriteAttribute, DefaultWriteAttributeHandler)
  DEF_DEFAULT_HANDLER(QueryOpcode::kReadFlag, DefaultReadFlagHandler)
  DEF_DEFAULT_HANDLER(QueryOpcode::kSetFlag, DefaultSetFlagHandler)
  DEF_DEFAULT_HANDLER(QueryOpcode::kToggleFlag, DefaultToggleFlagHandler)
  DEF_DEFAULT_HANDLER(QueryOpcode::kClearFlag, DefaultClearFlagHandler)
  DEF_DEFAULT_HANDLER_END()

 private:
  UfsMockDevice &mock_device_;
};

}  // namespace ufs_mock_device
}  // namespace ufs

#endif  // SRC_DEVICES_BLOCK_DRIVERS_UFS_TEST_MOCK_DEVICE_QUERY_REQUEST_PROCESSOR_H_
