// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BIN_DRIVER_MANAGER_DRIVER_HOST_RUNNER_H_
#define SRC_DEVICES_BIN_DRIVER_MANAGER_DRIVER_HOST_RUNNER_H_

#include <fidl/fuchsia.component.decl/cpp/fidl.h>
#include <fidl/fuchsia.component.runner/cpp/fidl.h>
#include <fidl/fuchsia.component/cpp/fidl.h>
#include <fidl/fuchsia.component/cpp/wire.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/fit/function.h>
#include <lib/zx/result.h>

#include <unordered_set>

#include "src/devices/lib/log/log.h"

namespace driver_manager {

class DriverHostRunner : public fidl::WireServer<fuchsia_component_runner::ComponentRunner> {
 public:
  DriverHostRunner(async_dispatcher_t* dispatcher, fidl::ClientEnd<fuchsia_component::Realm> realm);

  void PublishComponentRunner(component::OutgoingDirectory& outgoing);

  zx::result<> StartDriverHost();

 private:
  // The started component from the perspective of the Component Framework.
  struct StartedComponent {
    fuchsia_component_runner::ComponentStartInfo info;
    fidl::ServerEnd<fuchsia_component_runner::ComponentController> controller;
  };
  using StartCallback = fit::callback<void(zx::result<StartedComponent>)>;

  // fidl::WireServer<fuchsia_component_runner::ComponentRunner>
  void Start(StartRequestView request, StartCompleter::Sync& completer) override;

  void StartDriverHostComponent(std::string_view moniker, std::string_view url,
                                StartCallback callback);
  void LoadDriverHost(const fuchsia_component_runner::ComponentStartInfo& start_info);

  zx::result<> CallCallback(zx_koid_t koid, zx::result<StartedComponent> component);

  std::unordered_map<zx_koid_t, StartCallback> start_requests_;
  async_dispatcher_t* const dispatcher_;
  fidl::WireClient<fuchsia_component::Realm> realm_;
  fidl::ServerBindingGroup<fuchsia_component_runner::ComponentRunner> bindings_;

  uint64_t next_driver_host_id_ = 0;
};

}  // namespace driver_manager

#endif  // SRC_DEVICES_BIN_DRIVER_MANAGER_DRIVER_HOST_RUNNER_H_
