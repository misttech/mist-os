// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BIN_DRIVER_MANAGER_V2_RUNNER_H_
#define SRC_DEVICES_BIN_DRIVER_MANAGER_V2_RUNNER_H_

#include <fidl/fuchsia.component.decl/cpp/fidl.h>
#include <fidl/fuchsia.component.runner/cpp/fidl.h>
#include <fidl/fuchsia.component/cpp/fidl.h>
#include <fidl/fuchsia.driver.index/cpp/fidl.h>
#include <fidl/fuchsia.process/cpp/wire.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>

namespace driver_manager {

// This class serves as a runner for "driver" components. It also provides an API
// to allow driver components to be created in the current realm.
class Runner : public fidl::WireServer<fuchsia_component_runner::ComponentRunner> {
 public:
  // The started component from the perspective of the Component Framework.
  struct StartedComponent {
    fuchsia_component_runner::ComponentStartInfo info;
    fidl::ServerEnd<fuchsia_component_runner::ComponentController> controller;
  };
  using StartCallback = fit::callback<void(zx::result<StartedComponent>)>;

  Runner(async_dispatcher_t* dispatcher, fidl::WireClient<fuchsia_component::Realm> realm)
      : dispatcher_(dispatcher), realm_(std::move(realm)) {}

  zx::result<> Publish(component::OutgoingDirectory& outgoing);

  void StartDriverComponent(std::string_view moniker, std::string_view url,
                            std::string_view collection_name,
                            fidl::VectorView<fuchsia_component_decl::wire::Offer> offers,
                            StartCallback callback);

  const fidl::WireClient<fuchsia_component::Realm>& realm() const { return realm_; }

 private:
  // fidl::WireServer<fuchsia_component_runner::ComponentRunner>
  void Start(StartRequestView request, StartCompleter::Sync& completer) override;

  zx::result<> CallCallback(zx_koid_t koid, zx::result<StartedComponent> component);

  std::unordered_map<zx_koid_t, StartCallback> start_requests_;
  async_dispatcher_t* dispatcher_;
  fidl::WireClient<fuchsia_component::Realm> realm_;
  fidl::ServerBindingGroup<fuchsia_component_runner::ComponentRunner> bindings_;
};
}  // namespace driver_manager

#endif  // SRC_DEVICES_BIN_DRIVER_MANAGER_V2_RUNNER_H_
