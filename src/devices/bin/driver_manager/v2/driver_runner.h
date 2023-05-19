// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BIN_DRIVER_MANAGER_V2_DRIVER_RUNNER_H_
#define SRC_DEVICES_BIN_DRIVER_MANAGER_V2_DRIVER_RUNNER_H_

#include <fidl/fuchsia.component/cpp/wire.h>
#include <fidl/fuchsia.driver.development/cpp/wire.h>
#include <fidl/fuchsia.driver.host/cpp/wire.h>
#include <fidl/fuchsia.driver.index/cpp/wire.h>
#include <fidl/fuchsia.ldsvc/cpp/wire.h>
#include <lib/async/cpp/wait.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/fidl/cpp/wire/client.h>
#include <lib/fidl/cpp/wire/wire_messaging.h>
#include <lib/fit/function.h>
#include <lib/fpromise/promise.h>
#include <lib/inspect/cpp/inspect.h>
#include <lib/zircon-internal/thread_annotations.h>
#include <lib/zx/result.h>

#include <list>
#include <queue>
#include <unordered_map>
#include <unordered_set>

#include <fbl/intrusive_double_list.h>

#include "src/devices/bin/driver_manager/composite_node_spec/composite_manager_bridge.h"
#include "src/devices/bin/driver_manager/composite_node_spec/composite_node_spec_manager.h"
#include "src/devices/bin/driver_manager/inspect.h"
#include "src/devices/bin/driver_manager/v2/composite_assembler.h"
#include "src/devices/bin/driver_manager/v2/composite_node_spec_v2.h"
#include "src/devices/bin/driver_manager/v2/driver_host.h"
#include "src/devices/bin/driver_manager/v2/node.h"
#include "src/devices/bin/driver_manager/v2/node_removal_tracker.h"
#include "src/devices/bin/driver_manager/v2/node_remover.h"
#include "src/devices/bin/driver_manager/v2/runner.h"

// Note, all of the logic here assumes we are operating on a single-threaded
// dispatcher. It is not safe to use a multi-threaded dispatcher with this code.

namespace dfv2 {

class DriverRunner : public fidl::WireServer<fuchsia_driver_framework::CompositeNodeManager>,
                     public CompositeManagerBridge,
                     public NodeManager,
                     public NodeRemover {
  using LoaderServiceFactory = fit::function<zx::result<fidl::ClientEnd<fuchsia_ldsvc::Loader>>()>;

 public:
  DriverRunner(fidl::ClientEnd<fuchsia_component::Realm> realm,
               fidl::ClientEnd<fuchsia_driver_index::DriverIndex> driver_index,
               InspectManager& inspect, LoaderServiceFactory loader_service_factory,
               async_dispatcher_t* dispatcher);

  // fidl::WireServer<fuchsia_driver_framework::CompositeNodeManager> interface
  void AddSpec(AddSpecRequestView request, AddSpecCompleter::Sync& completer) override;

  // CompositeManagerBridge interface
  void BindNodesForCompositeNodeSpec() override;
  void AddSpecToDriverIndex(fuchsia_driver_framework::wire::CompositeNodeSpec group,
                            AddToIndexCallback callback) override;

  // NodeManager interface
  // Create a driver component with `url` against a given `node`.
  zx::result<> StartDriver(Node& node, std::string_view url,
                           fuchsia_driver_index::DriverPackageType package_type) override;

  // NodeManager interface
  // Shutdown hooks called by the shutdown manager
  void ShutdownAllDrivers(fit::callback<void()> callback) override {
    removal_tracker_.set_all_callback(std::move(callback));
    root_node_->Remove(RemovalSet::kAll, &removal_tracker_);
    removal_tracker_.FinishEnumeration();
  }

  void ShutdownPkgDrivers(fit::callback<void()> callback) override {
    removal_tracker_.set_pkg_callback(std::move(callback));
    root_node_->Remove(RemovalSet::kPackage, &removal_tracker_);
    removal_tracker_.FinishEnumeration();
  }

  void PublishComponentRunner(component::OutgoingDirectory& outgoing);
  void PublishCompositeNodeManager(component::OutgoingDirectory& outgoing);
  zx::result<> StartRootDriver(std::string_view url);

  // This function schedules a callback to attempt to bind all orphaned nodes against
  // the base drivers.
  void ScheduleBaseDriversBinding();
  // Goes through the orphan list and attempts the bind them again. Sends nodes that are still
  // orphaned back to the orphan list. Tracks the result of the bindings and then when finished
  // uses the result_callback to report the results.
  void TryBindAllAvailable(
      NodeBindingInfoResultCallback result_callback =
          [](fidl::VectorView<fuchsia_driver_development::wire::NodeBindingInfo>) {});

  zx::result<uint32_t> RestartNodesColocatedWithDriverUrl(std::string_view url);

  std::unordered_set<const DriverHost*> DriverHostsWithDriverUrl(std::string_view url);

  fpromise::promise<inspect::Inspector> Inspect() const;

  std::vector<fuchsia_driver_development::wire::CompositeInfo> GetCompositeListInfo(
      fidl::AnyArena& arena) const;

  size_t NumOrphanedNodes() const { return orphaned_nodes_.size(); }

  std::shared_ptr<Node> root_node() const { return root_node_; }

  // Only exposed for testing.
  CompositeNodeSpecManager& composite_node_spec_manager() { return composite_node_spec_manager_; }

  // Only exposed for testing.
  driver_manager::Runner& runner_for_tests() { return runner_; }

 private:
  using BindMatchCompleteCallback = fit::callback<void()>;

  struct BindRequest {
    std::weak_ptr<Node> node;
    std::string driver_url_suffix;
    std::shared_ptr<BindResultTracker> tracker;
    bool composite_only;
  };

  // NodeManager interface.
  // Attempt to bind `node`. A nullptr for result_tracker is acceptable if the caller doesn't intend
  // to track the results.
  void Bind(Node& node, std::shared_ptr<BindResultTracker> result_tracker) override;
  void BindToUrl(Node& node, std::string_view driver_url_suffix,
                 std::shared_ptr<BindResultTracker> result_tracker) override;
  void DestroyDriverComponent(Node& node, DestroyDriverComponentCallback callback) override;
  zx::result<DriverHost*> CreateDriverHost() override;

  // Should only be called when |bind_all_ongoing_| is true.
  void BindInternal(
      BindRequest request, BindMatchCompleteCallback match_complete_callback = []() {});

  // Should only be called when |bind_all_ongoing_| is true and |orphaned_nodes_| is not empty.
  void TryBindAllAvailableInternal(std::shared_ptr<BindResultTracker> tracker);

  size_t NumNodesAvailableForBind() const {
    return orphaned_nodes_.size() + composite_parents_.size();
  }

  // Callback function for a Driver Index match request.
  void OnMatchDriverCallback(
      BindRequest request,
      fidl::WireUnownedResult<fuchsia_driver_index::DriverIndex::MatchDriver>& result,
      BindMatchCompleteCallback match_complete_callback);

  // Binds |node| to |result. Returns a driver URL string if successful. Otherwise,
  // return std::nullopt.
  std::optional<std::string> BindNodeToResult(
      Node& node, fidl::WireUnownedResult<fuchsia_driver_index::DriverIndex::MatchDriver>& result,
      bool composite_only, bool has_tracker);

  zx::result<> BindNodeToSpec(Node& node,
                              fuchsia_driver_index::wire::MatchedCompositeNodeParentInfo parents);

  // Process any pending bind requests that were queued during an ongoing bind process.
  // Should only be called when |bind_all_ongoing_| is true.
  void ProcessPendingBindRequests();

  zx::result<std::string> StartDriver(Node& node,
                                      fuchsia_driver_index::wire::MatchedDriverInfo driver_info);

  zx::result<> CreateDriverHostComponent(std::string moniker,
                                         fidl::ServerEnd<fuchsia_io::Directory> exposed_dir);

  uint64_t next_driver_host_id_ = 0;
  fidl::WireClient<fuchsia_driver_index::DriverIndex> driver_index_;
  LoaderServiceFactory loader_service_factory_;
  fidl::ServerBindingGroup<fuchsia_component_runner::ComponentRunner> runner_bindings_;
  fidl::ServerBindingGroup<fuchsia_driver_framework::CompositeNodeManager> manager_bindings_;
  async_dispatcher_t* const dispatcher_;
  std::shared_ptr<Node> root_node_;

  // This is for dfv1 composite devices.
  CompositeDeviceManager composite_device_manager_;

  // This is for dfv2 composite node specs.
  CompositeNodeSpecManager composite_node_spec_manager_;

  driver_manager::Runner runner_;

  NodeRemovalTracker removal_tracker_;

  fbl::DoublyLinkedList<std::unique_ptr<DriverHostComponent>> driver_hosts_;

  // True when a call to TryBindAllAvailable() or Bind() was made and not yet completed.
  // Set to false by ProcessPendingBindRequests() when there are no more pending bind requests.
  bool bind_all_ongoing_ = false;

  // Queue of TryBindAllAvailable() callbacks pending for the next TryBindAllAvailable() trigger.
  std::vector<NodeBindingInfoResultCallback> pending_orphan_rebind_callbacks_;

  // Queue of Bind() calls that are made while there's an ongoing bind process. Once the process
  // is complete, ProcessPendingBindRequests() goes through the queue.
  std::queue<BindRequest> pending_bind_requests_;

  // Orphaned nodes are nodes that have failed to bind to a driver, either
  // because no matching driver could be found, or because the matching driver
  // failed to start. Maps the node's component moniker to its weak pointer.
  std::unordered_map<std::string, std::weak_ptr<Node>> orphaned_nodes_;

  // A list of composite node parents. In DFv1, a node can parent multiple composite
  // nodes. To follow that same behavior, we store the parents in a map to bind them
  // to other composites.
  // TODO(fxb/122531): Only add nodes that are enabled for composite multibind.
  std::unordered_map<std::string, std::weak_ptr<Node>> composite_parents_;
};

Collection ToCollection(const Node& node, fuchsia_driver_index::DriverPackageType package_type);

}  // namespace dfv2

#endif  // SRC_DEVICES_BIN_DRIVER_MANAGER_V2_DRIVER_RUNNER_H_
