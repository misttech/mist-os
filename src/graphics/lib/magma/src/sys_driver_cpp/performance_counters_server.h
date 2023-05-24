// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#ifndef SRC_GRAPHICS_LIB_MAGMA_SRC_SYS_DRIVER_CPP_PERFORMANCE_COUNTERS_SERVER_H_
#define SRC_GRAPHICS_LIB_MAGMA_SRC_SYS_DRIVER_CPP_PERFORMANCE_COUNTERS_SERVER_H_

#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <fidl/fuchsia.gpu.magma/cpp/wire.h>
#include <lib/driver/devfs/cpp/connector.h>
#include <lib/fdf/cpp/dispatcher.h>

#include "magma_util/macros.h"

class PerformanceCountersServer
    : public fidl::WireServer<fuchsia_gpu_magma::PerformanceCounterAccess> {
 public:
  PerformanceCountersServer()
      : devfs_connector_(fit::bind_member<&PerformanceCountersServer::BindConnector>(this)) {}

  zx::result<> Create(fidl::WireSyncClient<fuchsia_driver_framework::Node>& node_client);

  zx_koid_t GetEventKoid();

 private:
  void BindConnector(fidl::ServerEnd<fuchsia_gpu_magma::PerformanceCounterAccess> server) {
    fidl::BindServer(fdf::Dispatcher::GetCurrent()->async_dispatcher(), std::move(server), this);
  }

  void GetPerformanceCountToken(GetPerformanceCountTokenCompleter::Sync& completer) override;

  zx::event event_;
  driver_devfs::Connector<fuchsia_gpu_magma::PerformanceCounterAccess> devfs_connector_;
  fidl::WireSyncClient<fuchsia_driver_framework::Node> node_;
  fidl::WireSyncClient<fuchsia_driver_framework::NodeController> node_controller_;
};

#endif  // SRC_GRAPHICS_LIB_MAGMA_SRC_SYS_DRIVER_CPP_PERFORMANCE_COUNTERS_SERVER_H_
