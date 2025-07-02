// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#ifndef SRC_GRAPHICS_MAGMA_LIB_MAGMA_SERVICE_SYS_DRIVER_PERFORMANCE_COUNTERS_SERVER_H_
#define SRC_GRAPHICS_MAGMA_LIB_MAGMA_SERVICE_SYS_DRIVER_PERFORMANCE_COUNTERS_SERVER_H_

#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <fidl/fuchsia.gpu.magma/cpp/wire.h>
#include <lib/driver/devfs/cpp/connector.h>
#include <lib/driver/node/cpp/add_child.h>
#include <lib/fdf/cpp/dispatcher.h>
#include <lib/magma/util/macros.h>

namespace msd::internal {
class PerformanceCountersServer
    : public fidl::WireServer<fuchsia_gpu_magma::PerformanceCounterAccess> {
 public:
  explicit PerformanceCountersServer(async_dispatcher_t* dispatcher)
      : devfs_connector_(fit::bind_member<&PerformanceCountersServer::BindConnector>(this)),
        dispatcher_(dispatcher) {}

  zx::result<> Create(fidl::UnownedClientEnd<fuchsia_driver_framework::Node> parent);

  zx_koid_t GetEventKoid();

 private:
  void BindConnector(fidl::ServerEnd<fuchsia_gpu_magma::PerformanceCounterAccess> server) {
    fidl::BindServer(dispatcher_, std::move(server), this);
  }

  void GetPerformanceCountToken(GetPerformanceCountTokenCompleter::Sync& completer) override;

  zx::event event_;
  driver_devfs::Connector<fuchsia_gpu_magma::PerformanceCounterAccess> devfs_connector_;
  fdf::OwnedChildNode child_;
  async_dispatcher_t* dispatcher_;
};

}  // namespace msd::internal

#endif  // SRC_GRAPHICS_MAGMA_LIB_MAGMA_SERVICE_SYS_DRIVER_PERFORMANCE_COUNTERS_SERVER_H_
