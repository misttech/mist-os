// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/bind/bind_manager.h"

#include "src/devices/lib/log/log.h"

namespace fdd = fuchsia_driver_development;
namespace fdi = fuchsia_driver_index;

namespace driver_manager {

BindManager::BindManager(BindManagerBridge* bridge, NodeManager* node_manager,
                         async_dispatcher_t* dispatcher)
    : bridge_(bridge) {
  bind_node_set_.set_on_bind_state_changed([bridge]() { bridge->OnBindingStateChanged(); });
}

void BindManager::TryBindAllAvailable(NodeBindingInfoResultCallback result_callback) {
  // If there's an ongoing process to bind all orphans, queue up this callback. Once
  // the process is complete, it'll make another attempt to bind all orphans and invoke
  // all callbacks in the list.
  if (bind_node_set_.is_bind_ongoing()) {
    pending_orphan_rebind_callbacks_.push_back(std::move(result_callback));
    return;
  }

  if (bind_node_set_.NumOfAvailableNodes() == 0) {
    result_callback(fidl::VectorView<fuchsia_driver_development::wire::NodeBindingInfo>());
    return;
  }

  bind_node_set_.StartNextBindProcess();

  // In case there is a pending call to TryBindAllAvailable() after this one, we automatically
  // restart the process and call all queued up callbacks upon completion.
  auto next_attempt =
      [this, result_callback = std::move(result_callback)](
          fidl::VectorView<fuchsia_driver_development::wire::NodeBindingInfo> results) mutable {
        result_callback(results);
        ProcessPendingBindRequests();
      };
  std::shared_ptr<BindResultTracker> tracker = std::make_shared<BindResultTracker>(
      bind_node_set_.NumOfAvailableNodes(), std::move(next_attempt));
  TryBindAllAvailableInternal(tracker);
}

void BindManager::Bind(Node& node, std::string_view driver_url_suffix,
                       std::shared_ptr<BindResultTracker> result_tracker) {
  BindRequest request = {
      .node = node.weak_from_this(),
      .driver_url_suffix = std::string(driver_url_suffix),
      .tracker = result_tracker,
      .composite_only = false,
  };
  if (bind_node_set_.is_bind_ongoing()) {
    pending_bind_requests_.push_back(std::move(request));
    return;
  }

  // Remove the node from the orphaned nodes to avoid collision.
  bind_node_set_.RemoveOrphanedNode(node.MakeComponentMoniker());
  bind_node_set_.StartNextBindProcess();

  auto next_attempt = [this]() mutable { ProcessPendingBindRequests(); };
  BindInternal(std::move(request), next_attempt);
}

void BindManager::TryBindAllAvailableInternal(std::shared_ptr<BindResultTracker> tracker) {
  ZX_ASSERT(bind_node_set_.is_bind_ongoing());
  if (bind_node_set_.NumOfAvailableNodes() == 0) {
    return;
  }

  auto multibind_nodes = bind_node_set_.CurrentMultibindNodes();
  for (auto& [path, node_weak] : multibind_nodes) {
    std::shared_ptr node = node_weak.lock();
    if (!node) {
      tracker->ReportNoBind();
      continue;
    }

    BindInternal(BindRequest{
        .node = node_weak,
        .tracker = tracker,
        .composite_only = true,
    });
  }

  std::unordered_map<std::string, std::weak_ptr<Node>> orphaned_nodes =
      bind_node_set_.CurrentOrphanedNodes();
  for (auto& [path, node] : orphaned_nodes) {
    BindInternal(BindRequest{
        .node = node,
        .tracker = tracker,
        .composite_only = false,
    });
  }
}

void BindManager::BindInternal(BindRequest request,
                               BindMatchCompleteCallback match_complete_callback) {
  ZX_ASSERT(bind_node_set_.is_bind_ongoing());
  std::shared_ptr node = request.node.lock();
  if (!node) {
    LOGF(WARNING, "Node was freed before bind request is processed.");
    if (request.tracker) {
      request.tracker->ReportNoBind();
    }
    match_complete_callback();
    return;
  }

  std::string driver_url_suffix = request.driver_url_suffix;
  auto match_callback =
      [this, request = std::move(request),
       match_complete_callback = std::move(match_complete_callback)](
          fidl::WireUnownedResult<fdi::DriverIndex::MatchDriver>& result) mutable {
        OnMatchDriverCallback(std::move(request), result, std::move(match_complete_callback));
      };
  fidl::Arena arena;
  auto builder = fuchsia_driver_index::wire::MatchDriverArgs::Builder(arena).name(node->name());

  // Composite node's "default" node properties are its primary parent's node properties which
  // should not be used.
  if (node->type() == NodeType::kNormal) {
    auto node_properties = node->GetNodeProperties();
    if (node_properties.has_value()) {
      builder.properties(node_properties.value());
    }
  }
  if (!driver_url_suffix.empty()) {
    builder.driver_url_suffix(driver_url_suffix);
  }
  bridge_->RequestMatchFromDriverIndex(builder.Build(), std::move(match_callback));
}

void BindManager::OnMatchDriverCallback(
    BindRequest request, fidl::WireUnownedResult<fdi::DriverIndex::MatchDriver>& result,
    BindMatchCompleteCallback match_complete_callback) {
  auto report_no_bind = fit::defer([&request, &match_complete_callback]() mutable {
    if (request.tracker) {
      request.tracker->ReportNoBind();
    }
    match_complete_callback();
  });

  std::shared_ptr node = request.node.lock();

  // TODO(https://fxbug.dev/42075939): Add an additional guard to ensure that the node is still
  // available for binding when the match callback is fired. Currently, there are no issues from it,
  // but it is something we should address.
  if (!node) {
    LOGF(WARNING, "Node was freed before it could be bound");
    return;
  }

  BindResult bind_result =
      BindNodeToResult(*node, request.composite_only, result, request.tracker != nullptr);

  auto node_moniker = node->MakeComponentMoniker();

  // If the node fails to bind to anything, add it to the orphaned nodes.
  if (!bind_result.bound() && !request.composite_only &&
      !bind_node_set_.MultibindContains(node_moniker)) {
    bind_node_set_.AddOrphanedNode(*node);
    return;
  }

  // Remove bound nodes from the orphaned nodes.
  bind_node_set_.RemoveOrphanedNode(node_moniker);

  if (bind_result.bound()) {
    report_no_bind.cancel();
    if (request.tracker) {
      if (bind_result.is_driver_url()) {
        request.tracker->ReportSuccessfulBind(node_moniker, bind_result.driver_url());
      } else if (bind_result.is_composite_parents()) {
        request.tracker->ReportSuccessfulBind(node_moniker, bind_result.composite_parents());
      } else {
        LOGF(ERROR, "Unknown bind result type for %s.", node_moniker.c_str());
      }
    }

    match_complete_callback();
  }
}

BindResult BindManager::BindNodeToResult(
    Node& node, bool composite_only, fidl::WireUnownedResult<fdi::DriverIndex::MatchDriver>& result,
    bool has_tracker) {
  if (!result.ok()) {
    LOGF(ERROR, "Failed to call match Node '%s': %s", node.name().c_str(),
         result.error().FormatDescription().data());
    return BindResult();
  }

  if (result->is_error()) {
    // Log the failed MatchDriver only if we are not tracking the results with a tracker
    // or if the error is not a ZX_ERR_NOT_FOUND error (meaning it could not find a driver).
    // When we have a tracker, the bind is happening for all the orphan nodes and the
    // not found errors get very noisy.
    zx_status_t match_error = result->error_value();
    if (match_error != ZX_ERR_NOT_FOUND && !has_tracker) {
      LOGF(WARNING, "Failed to match Node '%s': %s", node.MakeTopologicalPath().c_str(),
           zx_status_get_string(match_error));
    }

    return BindResult();
  }

  auto& matched_driver = result->value();
  if (composite_only && !matched_driver->is_composite_parents()) {
    return BindResult();
  }

  if (!matched_driver->is_driver() && !matched_driver->is_composite_parents()) {
    LOGF(WARNING,
         "Failed to match Node '%s', the MatchedDriver is not a normal driver or a "
         "parent spec.",
         node.name().c_str());
    return BindResult();
  }

  if (matched_driver->is_composite_parents()) {
    fidl::Arena arena;
    auto result = BindNodeToSpec(arena, node, matched_driver->composite_parents());
    if (!result.is_ok()) {
      return BindResult();
    }

    auto owned_result = fidl::ToNatural(result.value());
    ZX_ASSERT(owned_result.has_value());
    return BindResult(owned_result.value());
  }

  ZX_ASSERT(matched_driver->is_driver());

  // If the node is already part of a composite, it should not bind to a driver.
  if (bind_node_set_.MultibindContains(node.MakeComponentMoniker())) {
    return BindResult();
  }

  auto start_result = bridge_->StartDriver(node, matched_driver->driver());
  if (start_result.is_error()) {
    LOGF(ERROR, "Failed to start driver '%s': %s", node.name().c_str(),
         zx_status_get_string(start_result.error_value()));
    return BindResult();
  }

  node.OnBind();
  return BindResult(start_result.value());
}

zx::result<CompositeParents> BindManager::BindNodeToSpec(fidl::AnyArena& arena, Node& node,
                                                         CompositeParents parents) {
  if (node.can_multibind_composites()) {
    bind_node_set_.AddOrMoveMultibindNode(node);
  }

  auto result = bridge_->BindToParentSpec(arena, parents, node.weak_from_this(),
                                          node.can_multibind_composites());
  if (result.is_error()) {
    if (result.error_value() != ZX_ERR_NOT_FOUND) {
      LOGF(ERROR, "Failed to bind node '%s' to any of the matched parent specs.",
           node.name().c_str());
    }
    return result.take_error();
  }

  for (auto& composite : result.value().completed_node_and_drivers) {
    std::shared_ptr composite_node = composite.node.lock();
    ZX_ASSERT(composite_node);
    auto start_result = bridge_->StartDriver(*composite_node, composite.driver.driver_info());
    if (start_result.is_error()) {
      LOGF(ERROR, "Failed to start driver '%s': %s", node.name().c_str(),
           zx_status_get_string(start_result.error_value()));
      continue;
    }
    composite_node->OnBind();
  }

  return zx::ok(result.value().bound_composite_parents);
}

void BindManager::ProcessPendingBindRequests() {
  ZX_ASSERT(bind_node_set_.is_bind_ongoing());
  if (pending_bind_requests_.empty() && pending_orphan_rebind_callbacks_.empty()) {
    bind_node_set_.EndBindProcess();
    return;
  }

  // Consolidate the pending bind requests and orphaned nodes to prevent collisions.
  for (auto& request : pending_bind_requests_) {
    if (auto node = request.node.lock(); node) {
      bind_node_set_.RemoveOrphanedNode(node->MakeComponentMoniker());
    }
  }

  // Begin the next bind process.
  bind_node_set_.StartNextBindProcess();

  bool have_bind_all_orphans_request = !pending_orphan_rebind_callbacks_.empty();
  size_t bind_tracker_size =
      have_bind_all_orphans_request
          ? pending_bind_requests_.size() + bind_node_set_.NumOfAvailableNodes()
          : pending_bind_requests_.size();

  // If there are no nodes to bind, then we'll run through all the callbacks and end the bind
  // process.
  if (have_bind_all_orphans_request && bind_tracker_size == 0) {
    for (auto& callback : pending_orphan_rebind_callbacks_) {
      fidl::Arena arena;
      callback(fidl::VectorView<fuchsia_driver_development::wire::NodeBindingInfo>(arena, 0));
    }
    pending_orphan_rebind_callbacks_.clear();
    bind_node_set_.EndBindProcess();
    return;
  }

  // Follow up with another ProcessPendingBindRequests() after all the pending bind calls are
  // complete. If there are no more accumulated bind calls, then the bind process ends.
  auto next_attempt =
      [this, callbacks = std::move(pending_orphan_rebind_callbacks_)](
          fidl::VectorView<fuchsia_driver_development::wire::NodeBindingInfo> results) mutable {
        for (auto& callback : callbacks) {
          callback(results);
        }
        ProcessPendingBindRequests();
      };

  std::shared_ptr<BindResultTracker> tracker =
      std::make_shared<BindResultTracker>(bind_tracker_size, std::move(next_attempt));

  // Go through all the pending bind requests.
  std::vector<BindRequest> pending_bind = std::move(pending_bind_requests_);
  for (auto& request : pending_bind) {
    auto match_complete_callback = [tracker]() mutable {
      // The bind status doesn't matter for this tracker.
      tracker->ReportNoBind();
    };
    BindInternal(std::move(request), std::move(match_complete_callback));
  }

  // If there are any pending callbacks for TryBindAllAvailable(), begin a new attempt.
  if (have_bind_all_orphans_request) {
    TryBindAllAvailableInternal(tracker);
  }
}

void BindManager::RecordInspect(inspect::Inspector& inspector) const {
  auto orphans = inspector.GetRoot().CreateChild("orphan_nodes");
  for (auto& [moniker, node] : bind_node_set_.CurrentOrphanedNodes()) {
    if (std::shared_ptr locked_node = node.lock()) {
      auto orphan = orphans.CreateChild(orphans.UniqueName("orphan-"));
      orphan.RecordString("moniker", moniker);
      orphans.Record(std::move(orphan));
    }
  }

  orphans.RecordBool("bind_all_ongoing", bind_node_set_.is_bind_ongoing());
  orphans.RecordUint("pending_bind_requests", pending_bind_requests_.size());
  orphans.RecordUint("pending_orphan_rebind_callbacks", pending_orphan_rebind_callbacks_.size());
  inspector.GetRoot().Record(std::move(orphans));
}

std::vector<fdd::wire::CompositeNodeInfo> BindManager::GetCompositeListInfo(
    fidl::AnyArena& arena) const {
  // TODO(https://fxbug.dev/42071016): Add composite node specs to the list.
  return {};
}

}  // namespace driver_manager
