// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BIN_DRIVER_MANAGER_BIND_BIND_MANAGER_H_
#define SRC_DEVICES_BIN_DRIVER_MANAGER_BIND_BIND_MANAGER_H_

#include <lib/fit/defer.h>
#include <lib/fit/function.h>

#include "src/devices/bin/driver_manager/bind/bind_node_set.h"
#include "src/devices/bin/driver_manager/bind/bind_result_tracker.h"
#include "src/devices/bin/driver_manager/composite_node_spec/composite_node_spec_manager.h"
#include "src/devices/bin/driver_manager/node.h"

namespace driver_manager {

using CompositeParents = fidl::VectorView<fuchsia_driver_framework::wire::CompositeParent>;
using OwnedCompositeParents = std::vector<fuchsia_driver_framework::CompositeParent>;

class DriverRunner;

struct BindRequest {
  std::string node_moniker;
  std::weak_ptr<Node> node;
  std::string driver_url_suffix;
  std::shared_ptr<BindResultTracker> tracker;
  bool composite_only;
};

class BindResult {
 public:
  BindResult() : data_(std::monostate{}) {}

  explicit BindResult(std::string_view driver_url) : data_(std::string(driver_url)) {}

  explicit BindResult(OwnedCompositeParents composite_parents)
      : data_(std::move(composite_parents)) {}

  bool bound() const { return !std::holds_alternative<std::monostate>(data_); }

  bool is_driver_url() const { return std::holds_alternative<std::string>(data_); }

  bool is_composite_parents() const { return std::holds_alternative<OwnedCompositeParents>(data_); }

  std::string_view driver_url() const {
    ZX_ASSERT(is_driver_url());
    return std::get<std::string>(data_);
  }

  const OwnedCompositeParents& composite_parents() const {
    ZX_ASSERT(is_composite_parents());
    return std::get<OwnedCompositeParents>(data_);
  }

 private:
  std::variant<std::monostate, std::string, OwnedCompositeParents> data_;
};

// Bridge class for driver manager related interactions.
class BindManagerBridge {
 public:
  virtual zx::result<BindSpecResult> BindToParentSpec(fidl::AnyArena& arena,
                                                      CompositeParents composite_parents,
                                                      std::weak_ptr<Node> node,
                                                      bool enable_multibind) = 0;

  virtual zx::result<std::string> StartDriver(
      Node& node, fuchsia_driver_framework::wire::DriverInfo driver_info) = 0;

  virtual void RequestMatchFromDriverIndex(
      fuchsia_driver_index::wire::MatchDriverArgs args,
      fit::callback<void(fidl::WireUnownedResult<fuchsia_driver_index::DriverIndex::MatchDriver>&)>
          match_callback) = 0;

  virtual void OnBindingStateChanged() {}
};

// This class is responsible for managing driver binding.
class BindManager {
 public:
  explicit BindManager(BindManagerBridge* bridge, NodeManager* node_manager,
                       async_dispatcher_t* dispatcher);

  void Bind(Node& node, std::string_view driver_url_suffix,
            std::shared_ptr<BindResultTracker> result_tracker);

  void TryBindAllAvailable(
      NodeBindingInfoResultCallback result_callback =
          [](fidl::VectorView<fuchsia_driver_development::wire::NodeBindingInfo>) {});

  void RecordInspect(inspect::Inspector& inspector) const;

  std::vector<fuchsia_driver_development::wire::CompositeNodeInfo> GetCompositeListInfo(
      fidl::AnyArena& arena) const;

  // Exposed for testing.
  size_t NumOrphanedNodes() const { return bind_node_set_.NumOfOrphanedNodes(); }

  bool HasOngoingBind() const { return bind_node_set_.is_bind_ongoing(); }

 protected:
  // Exposed for testing.
  const BindNodeSet& bind_node_set() const { return bind_node_set_; }

  // Exposed for testing.
  std::vector<BindRequest> pending_bind_requests() const { return pending_bind_requests_; }

  // Exposed for testing.
  const std::vector<NodeBindingInfoResultCallback>& pending_orphan_rebind_callbacks() const {
    return pending_orphan_rebind_callbacks_;
  }

 private:
  using BindMatchCompleteCallback = fit::callback<void()>;

  // Should only be called when |bind_node_set_.is_bind_ongoing()| is true.
  void BindInternal(
      BindRequest request, BindMatchCompleteCallback match_complete_callback = []() {});

  // Should only be called when |bind_node_set_.is_bind_ongoing()| is true and |orphaned_nodes_| is
  // not empty.
  void TryBindAllAvailableInternal(std::shared_ptr<BindResultTracker> tracker);

  // Process any pending bind requests that were queued during an ongoing bind process.
  // Should only be called when |bind_node_set_.is_bind_ongoing()| is true.
  void ProcessPendingBindRequests();

  // Callback function for a Driver Index match request.
  void OnMatchDriverCallback(
      BindRequest request,
      fidl::WireUnownedResult<fuchsia_driver_index::DriverIndex::MatchDriver>& result,
      BindMatchCompleteCallback match_complete_callback);

  // Binds |node| to |result|.
  // Result contains a vector of composite spec info that the node binded to if it matched composite
  // spec parents, or it will have a string with the driver URL if it matched directly to a driver.
  BindResult BindNodeToResult(
      Node& node, bool composite_only,
      fidl::WireUnownedResult<fuchsia_driver_index::DriverIndex::MatchDriver>& result,
      bool has_tracker);

  zx::result<CompositeParents> BindNodeToSpec(fidl::AnyArena& arena, Node& node,
                                              CompositeParents composite_parents);

  // Queue of TryBindAllAvailable() callbacks pending for the next TryBindAllAvailable() trigger.
  std::vector<NodeBindingInfoResultCallback> pending_orphan_rebind_callbacks_;

  // Queue of Bind() calls that are made while there's an ongoing bind process. Once the process
  // is complete, ProcessPendingBindRequests() goes through the queue.
  std::vector<BindRequest> pending_bind_requests_;

  BindNodeSet bind_node_set_;

  // Must outlive BindManager.
  BindManagerBridge* bridge_;
};

}  // namespace driver_manager

#endif  // SRC_DEVICES_BIN_DRIVER_MANAGER_BIND_BIND_MANAGER_H_
