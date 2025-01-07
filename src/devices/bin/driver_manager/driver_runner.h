// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BIN_DRIVER_MANAGER_DRIVER_RUNNER_H_
#define SRC_DEVICES_BIN_DRIVER_MANAGER_DRIVER_RUNNER_H_

#include <fidl/fuchsia.component/cpp/wire.h>
#include <fidl/fuchsia.driver.development/cpp/wire.h>
#include <fidl/fuchsia.driver.host/cpp/wire.h>
#include <fidl/fuchsia.driver.index/cpp/wire.h>
#include <fidl/fuchsia.driver.token/cpp/fidl.h>
#include <fidl/fuchsia.ldsvc/cpp/wire.h>
#include <lib/async/cpp/wait.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/fit/function.h>
#include <lib/fpromise/promise.h>
#include <lib/inspect/cpp/inspect.h>
#include <lib/zircon-internal/thread_annotations.h>

#include <unordered_set>

#include <fbl/intrusive_double_list.h>

#include "src/devices/bin/driver_loader/loader.h"
#include "src/devices/bin/driver_manager/bind/bind_manager.h"
#include "src/devices/bin/driver_manager/bootup_tracker.h"
#include "src/devices/bin/driver_manager/composite_node_spec/composite_manager_bridge.h"
#include "src/devices/bin/driver_manager/composite_node_spec/composite_node_spec_manager.h"
#include "src/devices/bin/driver_manager/driver_host.h"
#include "src/devices/bin/driver_manager/driver_host_runner.h"
#include "src/devices/bin/driver_manager/inspect.h"
#include "src/devices/bin/driver_manager/node.h"
#include "src/devices/bin/driver_manager/runner.h"
#include "src/devices/bin/driver_manager/shutdown/node_removal_tracker.h"
#include "src/devices/bin/driver_manager/shutdown/node_remover.h"
#include "src/devices/lib/log/log.h"

// Note, all of the logic here assumes we are operating on a single-threaded
// dispatcher. It is not safe to use a multi-threaded dispatcher with this code.

namespace driver_manager {

class DriverRunner : public fidl::WireServer<fuchsia_driver_framework::CompositeNodeManager>,
                     public fidl::WireServer<fuchsia_driver_index::DriverNotifier>,
                     public fidl::Server<fuchsia_driver_token::NodeBusTopology>,
                     public BindManagerBridge,
                     public CompositeManagerBridge,
                     public NodeManager,
                     public NodeRemover {
  using LoaderServiceFactory = fit::function<zx::result<fidl::ClientEnd<fuchsia_ldsvc::Loader>>()>;

  using DynamicLinkerServiceFactory =
      fit::function<zx::result<fidl::ClientEnd<fuchsia_driver_loader::DriverHostLauncher>>()>;

 public:
  // Args required to enable dynamic linking
  struct DynamicLinkerArgs {
    DynamicLinkerServiceFactory linker_service_factory;
    std::unique_ptr<DriverHostRunner> driver_host_runner;
  };

  // |Dynamic_linker_args| should be set if dynamic linking is available.
  DriverRunner(fidl::ClientEnd<fuchsia_component::Realm> realm,
               fidl::ClientEnd<fuchsia_driver_index::DriverIndex> driver_index,
               InspectManager& inspect, LoaderServiceFactory loader_service_factory,
               async_dispatcher_t* dispatcher, bool enable_test_shutdown_delays,
               std::optional<DynamicLinkerArgs> dynamic_linker_args = std::nullopt);

  // fidl::WireServer<fuchsia_driver_framework::CompositeNodeManager> interface
  void AddSpec(AddSpecRequestView request, AddSpecCompleter::Sync& completer) override;

  // fidl::WireServer<fuchsia_driver_index::DriverNotifier>
  void NewDriverAvailable(NewDriverAvailableCompleter::Sync& completer) override;

  void handle_unknown_method(
      fidl::UnknownMethodMetadata<fuchsia_driver_framework::CompositeNodeManager> metadata,
      fidl::UnknownMethodCompleter::Sync& completer) override;

  // fidl::WireServer<fuchsia_driver_token::NodeBusTopology>
  void Get(GetRequest& request, GetCompleter::Sync& completer) override;

  void handle_unknown_method(
      fidl::UnknownMethodMetadata<fuchsia_driver_token::NodeBusTopology> metadata,
      fidl::UnknownMethodCompleter::Sync& completer) override;

  // CompositeManagerBridge interface
  void BindNodesForCompositeNodeSpec() override;
  void AddSpecToDriverIndex(fuchsia_driver_framework::wire::CompositeNodeSpec group,
                            AddToIndexCallback callback) override;

  // NodeManager interface
  // Create a driver component with `url` against a given `node`.
  zx::result<> StartDriver(Node& node, std::string_view url,
                           fuchsia_driver_framework::DriverPackageType package_type) override;

  // NodeManager interface
  // Shutdown hooks called by the shutdown manager
  void ShutdownAllDrivers(fit::callback<void()> callback) override {
    LOGF(INFO, "Driver Runner invokes shutdown all drivers");
    removal_tracker_.set_all_callback(std::move(callback));
    root_node_->Remove(RemovalSet::kAll, &removal_tracker_);
    removal_tracker_.FinishEnumeration();
  }

  void ShutdownPkgDrivers(fit::callback<void()> callback) override {
    removal_tracker_.set_pkg_callback(std::move(callback));
    root_node_->Remove(RemovalSet::kPackage, &removal_tracker_);
    removal_tracker_.FinishEnumeration();
  }

  void RebindComposite(std::string spec, std::optional<std::string> driver_url,
                       fit::callback<void(zx::result<>)> callback) override;

  bool IsTestShutdownDelayEnabled() const override { return enable_test_shutdown_delays_; }
  std::weak_ptr<std::mt19937> GetShutdownTestRng() const override {
    return shutdown_test_delay_rng_;
  }

  void PublishComponentRunner(component::OutgoingDirectory& outgoing);
  zx::result<> StartRootDriver(std::string_view url);

  // Goes through the orphan list and attempts the bind them again. Sends nodes that are still
  // orphaned back to the orphan list. Tracks the result of the bindings and then when finished
  // uses the result_callback to report the results.
  void TryBindAllAvailable(
      NodeBindingInfoResultCallback result_callback =
          [](fidl::VectorView<fuchsia_driver_development::wire::NodeBindingInfo>) {});

  // Restarts all the nodes that are colocated with a driver with the given |url|.
  zx::result<uint32_t> RestartNodesColocatedWithDriverUrl(
      std::string_view url, fuchsia_driver_development::RestartRematchFlags rematch_flags);

  std::unordered_set<const DriverHost*> DriverHostsWithDriverUrl(std::string_view url);

  fpromise::promise<inspect::Inspector> Inspect() const;

  std::vector<fuchsia_driver_development::wire::CompositeNodeInfo> GetCompositeListInfo(
      fidl::AnyArena& arena) const;

  void WaitForBootup(fit::callback<void()> callback);

  fidl::WireClient<fuchsia_driver_index::DriverIndex>& driver_index() { return driver_index_; }

  std::shared_ptr<Node> root_node() const { return root_node_; }

  // Only exposed for testing.
  CompositeNodeSpecManager& composite_node_spec_manager() { return composite_node_spec_manager_; }
  const BindManager& bind_manager() const { return bind_manager_; }
  driver_manager::Runner& runner_for_tests() { return runner_; }

  driver_manager::DriverHostRunner* driver_host_runner_for_tests() {
    return dynamic_linker_args_.has_value() ? dynamic_linker_args_->driver_host_runner.get()
                                            : nullptr;
  }

 private:
  // NodeManager interface.
  // Attempt to bind `node`. A nullptr for result_tracker is acceptable if the caller doesn't intend
  // to track the results.
  void Bind(Node& node, std::shared_ptr<BindResultTracker> result_tracker) override;
  void BindToUrl(Node& node, std::string_view driver_url_suffix,
                 std::shared_ptr<BindResultTracker> result_tracker) override;
  void DestroyDriverComponent(Node& node, DestroyDriverComponentCallback callback) override;
  zx::result<DriverHost*> CreateDriverHost(bool use_next_vdso) override;
  // Creates the driver host component, loads the driver host using dynamic linking,
  // and calls |cb| on completion. |cb| will only be called if the return value is zx::ok.
  void CreateDriverHostDynamicLinker(
      fit::callback<void(zx::result<DriverHost*>)> completion_cb) override;
  bool IsDriverHostValid(DriverHost* driver_host) const override;

  // BindManagerBridge interface.
  zx::result<std::string> StartDriver(
      Node& node, fuchsia_driver_framework::wire::DriverInfo driver_info) override;
  zx::result<BindSpecResult> BindToParentSpec(fidl::AnyArena& arena,
                                              CompositeParents composite_parents,
                                              std::weak_ptr<Node> node,
                                              bool enable_multibind) override;
  void RequestMatchFromDriverIndex(
      fuchsia_driver_index::wire::MatchDriverArgs args,
      fit::callback<void(fidl::WireUnownedResult<fuchsia_driver_index::DriverIndex::MatchDriver>&)>
          match_callback) override;
  void RequestRebindFromDriverIndex(std::string spec, std::optional<std::string> driver_url_suffix,
                                    fit::callback<void(zx::result<>)> callback) override;

  void OnBindingStateChanged() override { bootup_tracker_->NotifyBindingChanged(); }

  zx::result<> CreateDriverHostComponent(std::string moniker,
                                         fidl::ServerEnd<fuchsia_io::Directory> exposed_dir,
                                         std::shared_ptr<bool> exposed_dir_connected,
                                         bool use_next_vdso);

  uint64_t next_driver_host_id_ = 0;
  fidl::WireClient<fuchsia_driver_index::DriverIndex> driver_index_;
  LoaderServiceFactory loader_service_factory_;
  fidl::ServerBindingGroup<fuchsia_driver_framework::CompositeNodeManager> manager_bindings_;
  fidl::ServerBindingGroup<fuchsia_driver_token::NodeBusTopology> bus_topo_bindings_;
  fidl::ServerBindingGroup<fuchsia_driver_index::DriverNotifier> driver_notifier_bindings_;
  async_dispatcher_t* const dispatcher_;
  std::shared_ptr<Node> root_node_;

  // Manages composite node specs.
  CompositeNodeSpecManager composite_node_spec_manager_;

  // Manages driver binding.
  BindManager bind_manager_;

  driver_manager::Runner runner_;

  NodeRemovalTracker removal_tracker_;

  std::shared_ptr<BootupTracker> bootup_tracker_;

  fbl::DoublyLinkedList<std::unique_ptr<DriverHostComponent>> driver_hosts_;

  // True if the driver manager should inject test delays in the shutdown process. Set by the
  // structured config.
  bool enable_test_shutdown_delays_;

  // RNG engine for the shutdown test delays. For reproducibility reasons, only one engine should
  // be used.
  std::shared_ptr<std::mt19937> shutdown_test_delay_rng_;

  // Set if dynamic linking is available.
  std::optional<DynamicLinkerArgs> dynamic_linker_args_;

  // TODO(https://fxbug.dev/349831408): for now we use the same dynamic linker client
  // channel for each driver host.
  std::optional<fidl::WireSharedClient<fuchsia_driver_loader::DriverHostLauncher>>
      driver_host_launcher_;
};

Collection ToCollection(const Node& node, fuchsia_driver_framework::DriverPackageType package_type);

}  // namespace driver_manager

#endif  // SRC_DEVICES_BIN_DRIVER_MANAGER_DRIVER_RUNNER_H_
