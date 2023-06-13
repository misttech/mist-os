// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BIN_DRIVER_MANAGER_V2_BIND_MANAGER_H_
#define SRC_DEVICES_BIN_DRIVER_MANAGER_V2_BIND_MANAGER_H_

#include <lib/fit/defer.h>
#include <lib/fit/function.h>

#include <queue>
#include <unordered_map>

#include "src/devices/bin/driver_manager/composite_node_spec/composite_node_spec_manager.h"
#include "src/devices/bin/driver_manager/v2/composite_assembler.h"

namespace dfv2 {

class DriverRunner;

struct BindRequest {
  std::weak_ptr<Node> node;
  std::string driver_url_suffix;
  std::shared_ptr<BindResultTracker> tracker;
};

// Bridge class for driver manager related interactions.
class BindManagerBridge {
 public:
  virtual zx::result<std::vector<CompositeNodeAndDriver>> BindToParentSpec(
      fuchsia_driver_index::wire::MatchedCompositeNodeParentInfo match_info,
      std::weak_ptr<Node> node, bool enable_multibind) = 0;

  virtual zx::result<std::string> StartDriver(
      Node& node, fuchsia_driver_index::wire::MatchedDriverInfo driver_info) = 0;

  virtual void RequestMatchFromDriverIndex(
      fuchsia_driver_index::wire::MatchDriverArgs args,
      fit::callback<void(fidl::WireUnownedResult<fuchsia_driver_index::DriverIndex::MatchDriver>&)>
          match_callback) = 0;
};

// This class is responsible for managing driver binding.
class BindManager {
 public:
  explicit BindManager(BindManagerBridge* bridge, NodeManager* node_manager,
                       async_dispatcher_t* dispatcher);

  // Publish capabilities to the outgoing directory.
  void Publish(component::OutgoingDirectory& outgoing);

  void Bind(Node& node, std::string_view driver_url_suffix,
            std::shared_ptr<BindResultTracker> result_tracker);

  void TryBindAllAvailable(
      NodeBindingInfoResultCallback result_callback =
          [](fidl::VectorView<fuchsia_driver_development::wire::NodeBindingInfo>) {});

  void RecordInspect(inspect::Inspector& inspector) const;

  std::vector<fuchsia_driver_development::wire::CompositeInfo> GetCompositeListInfo(
      fidl::AnyArena& arena) const;

  // Exposed for testing.
  size_t NumOrphanedNodes() const { return orphaned_nodes_.size(); }

 protected:
  // Exposed for testing.
  const std::unordered_map<std::string, std::weak_ptr<Node>>& orphaned_nodes() const {
    return orphaned_nodes_;
  }

  // Exposed for testing.
  bool bind_all_ongoing() const { return bind_all_ongoing_; }

  // Exposed for testing.
  std::vector<BindRequest> pending_bind_requests() const { return pending_bind_requests_; }

  // Exposed for testing.
  const std::vector<NodeBindingInfoResultCallback>& pending_orphan_rebind_callbacks() const {
    return pending_orphan_rebind_callbacks_;
  }

  // Exposed for testing.
  CompositeDeviceManager& legacy_composite_manager() { return legacy_composite_manager_; }

 private:
  using BindMatchCompleteCallback = fit::callback<void()>;

  // Should only be called when |bind_all_ongoing_| is true.
  void BindInternal(
      BindRequest request, BindMatchCompleteCallback match_complete_callback = []() {});

  // Should only be called when |bind_all_ongoing_| is true and |orphaned_nodes_| is not empty.
  void TryBindAllAvailableInternal(std::shared_ptr<BindResultTracker> tracker);

  // Process any pending bind requests that were queued during an ongoing bind process.
  // Should only be called when |bind_all_ongoing_| is true.
  void ProcessPendingBindRequests();

  // Callback function for a Driver Index match request.
  void OnMatchDriverCallback(
      BindRequest request,
      fidl::WireUnownedResult<fuchsia_driver_index::DriverIndex::MatchDriver>& result,
      BindMatchCompleteCallback match_complete_callback);

  // Binds |node| to |result. Returns a driver URL string if successful. Otherwise,
  // return std::nullopt.
  std::optional<std::string> BindNodeToResult(
      Node& node, fidl::WireUnownedResult<fuchsia_driver_index::DriverIndex::MatchDriver>& result,
      bool has_tracker);

  zx::result<> BindNodeToSpec(Node& node,
                              fuchsia_driver_index::wire::MatchedCompositeNodeParentInfo parents);

  // True when a call to TryBindAllAvailable() or Bind() was made and not yet completed.
  // Set to false by ProcessPendingBindRequests() when there are no more pending bind requests.
  bool bind_all_ongoing_ = false;

  // Queue of TryBindAllAvailable() callbacks pending for the next TryBindAllAvailable() trigger.
  std::vector<NodeBindingInfoResultCallback> pending_orphan_rebind_callbacks_;

  // Queue of Bind() calls that are made while there's an ongoing bind process. Once the process
  // is complete, ProcessPendingBindRequests() goes through the queue.
  std::vector<BindRequest> pending_bind_requests_;

  // Orphaned nodes are nodes that have failed to bind to a driver, either
  // because no matching driver could be found, or because the matching driver
  // failed to start. Maps the node's component moniker to its weak pointer.
  std::unordered_map<std::string, std::weak_ptr<Node>> orphaned_nodes_;

  // Manages DFv1 legacy composites.
  CompositeDeviceManager legacy_composite_manager_;

  // Must outlive BindManager.
  BindManagerBridge* bridge_;
};

}  // namespace dfv2

#endif  // SRC_DEVICES_BIN_DRIVER_MANAGER_V2_BIND_MANAGER_H_
