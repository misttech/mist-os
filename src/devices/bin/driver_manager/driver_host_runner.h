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

#include <fbl/intrusive_double_list.h>

#include "src/devices/bin/driver_loader/loader.h"
#include "src/devices/lib/log/log.h"

namespace driver_manager {

class DriverHostRunner : public fidl::WireServer<fuchsia_component_runner::ComponentRunner> {
 public:
  class DriverHost : public fbl::DoublyLinkedListable<std::unique_ptr<DriverHost>> {
   public:
    DriverHost(zx::process process, zx::thread thread, zx::vmar root_vmar)
        : process_(std::move(process)),
          thread_(std::move(thread)),
          root_vmar_(std::move(root_vmar)) {}

    // Returns duplicate handles that can be passed to the loader process.
    zx_status_t GetDuplicateHandles(zx::process* out_process, zx::thread* out_thread,
                                    zx::vmar* out_root_vmar);

    const zx::process& process() const { return process_; }

   private:
    zx::process process_;
    zx::thread thread_;
    zx::vmar root_vmar_;
  };

  // TODO(https://fxbug.dev/340928556): start the loader as a separate process instead.
  DriverHostRunner(async_dispatcher_t* dispatcher, fidl::ClientEnd<fuchsia_component::Realm> realm,
                   std::unique_ptr<driver_loader::Loader> loader);

  void PublishComponentRunner(component::OutgoingDirectory& outgoing);

  zx::result<> StartDriverHost();

  // Returns all started driver hosts. This will be used by tests.
  std::unordered_set<const DriverHost*> DriverHosts();

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
  void LoadDriverHost(const fuchsia_component_runner::ComponentStartInfo& start_info,
                      std::string_view name);

  // Creates the process and starting thread for a driver host.
  zx::result<DriverHost*> CreateDriverHost(std::string_view name);

  zx::result<> CallCallback(zx_koid_t koid, zx::result<StartedComponent> component);

  std::unordered_map<zx_koid_t, StartCallback> start_requests_;
  async_dispatcher_t* const dispatcher_;
  fidl::WireClient<fuchsia_component::Realm> realm_;
  fidl::ServerBindingGroup<fuchsia_component_runner::ComponentRunner> bindings_;

  uint64_t next_driver_host_id_ = 0;
  fbl::DoublyLinkedList<std::unique_ptr<DriverHost>> driver_hosts_;

  // TODO(https://fxbug.dev/340928556): start the loader as a separate process instead.
  std::unique_ptr<driver_loader::Loader> loader_;
};

}  // namespace driver_manager

#endif  // SRC_DEVICES_BIN_DRIVER_MANAGER_DRIVER_HOST_RUNNER_H_
