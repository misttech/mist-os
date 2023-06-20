// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/v2/bind_manager.h"

#include "src/devices/lib/log/log.h"

namespace fdd = fuchsia_driver_development;
namespace fdi = fuchsia_driver_index;

namespace dfv2 {

BindManager::BindManager(BindManagerBridge* bridge, NodeManager* node_manager,
                         async_dispatcher_t* dispatcher)
    : legacy_composite_manager_(node_manager, dispatcher,
                                [this]() { this->TryBindAllAvailable(); }),
      bridge_(bridge) {}

void BindManager::Publish(component::OutgoingDirectory& outgoing) {
  legacy_composite_manager_.Publish(outgoing);
}

void BindManager::TryBindAllAvailable(NodeBindingInfoResultCallback result_callback) {
  // If there's an ongoing process to bind all orphans, queue up this callback. Once
  // the process is complete, it'll make another attempt to bind all orphans and invoke
  // all callbacks in the list.
  if (bind_all_ongoing_) {
    pending_orphan_rebind_callbacks_.push_back(std::move(result_callback));
    return;
  }

  if (orphaned_nodes_.empty() && composite_parents_.empty()) {
    result_callback(fidl::VectorView<fuchsia_driver_development::wire::NodeBindingInfo>());
    return;
  }

  bind_all_ongoing_ = true;

  // In case there is a pending call to TryBindAllAvailable() after this one, we automatically
  // restart the process and call all queued up callbacks upon completion.
  auto next_attempt =
      [this, result_callback = std::move(result_callback)](
          fidl::VectorView<fuchsia_driver_development::wire::NodeBindingInfo> results) mutable {
        result_callback(results);
        ProcessPendingBindRequests();
      };
  std::shared_ptr<BindResultTracker> tracker =
      std::make_shared<BindResultTracker>(orphaned_nodes_.size(), std::move(next_attempt));
  TryBindAllAvailableInternal(tracker);
}

void BindManager::Bind(Node& node, std::string_view driver_url_suffix,
                       std::shared_ptr<BindResultTracker> result_tracker) {
  BindRequest request = {
      .node = node.weak_from_this(),
      .driver_url_suffix = std::string(driver_url_suffix),
      .tracker = result_tracker,
  };
  if (bind_all_ongoing_) {
    pending_bind_requests_.push_back(std::move(request));
    return;
  }

  // Remove the node from |orphaned_nodes_| to avoid collision.
  orphaned_nodes_.erase(node.MakeComponentMoniker());

  bind_all_ongoing_ = true;

  auto next_attempt = [this]() mutable { ProcessPendingBindRequests(); };
  BindInternal(std::move(request), next_attempt);
}

void BindManager::TryBindAllAvailableInternal(std::shared_ptr<BindResultTracker> tracker) {
  ZX_ASSERT(bind_all_ongoing_);
  if (orphaned_nodes_.empty() && composite_parents_.empty()) {
    return;
  }

  auto cached_parents = std::move(composite_parents_);
  for (auto& [path, node_weak] : cached_parents) {
    if (auto node = node_weak.lock(); node) {
      legacy_composite_manager_.BindNode(node);
    }
    composite_parents_.emplace(path, node_weak);
  }

  // Clear our stored map of orphaned nodes. It will be repopulated in BindInternal().
  std::unordered_map<std::string, std::weak_ptr<Node>> orphaned_nodes = std::move(orphaned_nodes_);
  orphaned_nodes_ = {};
  for (auto& [path, node] : orphaned_nodes) {
    BindInternal(BindRequest{
        .node = node,
        .tracker = tracker,
    });
  }
}

void BindManager::BindInternal(BindRequest request,
                               BindMatchCompleteCallback match_complete_callback) {
  ZX_ASSERT(bind_all_ongoing_);
  std::shared_ptr node = request.node.lock();
  if (!node) {
    LOGF(WARNING, "Node was freed before bind request is processed.");
    if (request.tracker) {
      request.tracker->ReportNoBind();
    }
    match_complete_callback();
    return;
  }

  // Check the DFv1 composites first, and don't bind to others if they match.
  if (legacy_composite_manager_.BindNode(node)) {
    composite_parents_.emplace(node->MakeComponentMoniker(), request.node);
    if (request.tracker) {
      request.tracker->ReportSuccessfulBind(node->MakeComponentMoniker(), "");
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
  auto builder = fuchsia_driver_index::wire::MatchDriverArgs::Builder(arena)
                     .name(node->name())
                     .properties(node->properties());
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

  // TODO(fxb/125100): Add an additional guard to ensure that the node is still available for
  // binding when the match callback is fired. Currently, there are no issues from it, but it
  // is something we should address.
  if (!node) {
    LOGF(WARNING, "Node was freed before it could be bound");
    report_no_bind.call();
    return;
  }

  auto driver_url = BindNodeToResult(*node, result, request.tracker != nullptr);
  if (driver_url == std::nullopt) {
    orphaned_nodes_.emplace(node->MakeComponentMoniker(), node);
    report_no_bind.call();
    return;
  }

  report_no_bind.cancel();
  if (request.tracker) {
    request.tracker->ReportSuccessfulBind(node->MakeComponentMoniker(), driver_url.value());
  }
  match_complete_callback();
}

std::optional<std::string> BindManager::BindNodeToResult(
    Node& node, fidl::WireUnownedResult<fdi::DriverIndex::MatchDriver>& result, bool has_tracker) {
  if (!result.ok()) {
    LOGF(ERROR, "Failed to call match Node '%s': %s", node.name().c_str(),
         result.error().FormatDescription().data());
    return std::nullopt;
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

    return std::nullopt;
  }

  auto& matched_driver = result->value()->driver;
  if (!matched_driver.is_driver() && !matched_driver.is_parent_spec()) {
    LOGF(WARNING,
         "Failed to match Node '%s', the MatchedDriver is not a normal driver or a "
         "parent spec.",
         node.name().c_str());
    return std::nullopt;
  }

  if (matched_driver.is_parent_spec() && !matched_driver.parent_spec().has_specs()) {
    LOGF(WARNING,
         "Failed to match Node '%s', the MatchedDriver is missing the composite node specs in the "
         "parent spec.",
         node.name().c_str());
    return std::nullopt;
  }

  if (matched_driver.is_parent_spec()) {
    auto result = BindNodeToSpec(node, matched_driver.parent_spec());
    if (!result.is_ok()) {
      return std::nullopt;
    }
    return "";
  }

  ZX_ASSERT(matched_driver.is_driver());
  auto start_result = bridge_->StartDriver(node, matched_driver.driver());
  if (start_result.is_error()) {
    LOGF(ERROR, "Failed to start driver '%s': %s", node.name().c_str(),
         zx_status_get_string(start_result.error_value()));
    return std::nullopt;
  }

  node.OnBind();
  return start_result.value();
}

zx::result<> BindManager::BindNodeToSpec(
    Node& node, fuchsia_driver_index::wire::MatchedCompositeNodeParentInfo parents) {
  auto result = bridge_->BindToParentSpec(parents, node.weak_from_this(), false);
  if (result.is_error()) {
    LOGF(ERROR, "Failed to bind node '%s' to any of the matched parent specs.",
         node.name().c_str());
    return result.take_error();
  }

  auto composite_list = result.value();
  if (composite_list.empty()) {
    return zx::ok();
  }

  // TODO(fxb/122531): Support composite multibind.
  ZX_ASSERT(composite_list.size() == 1u);
  auto composite_node_and_driver = composite_list[0];

  auto composite_node = std::get<std::weak_ptr<dfv2::Node>>(composite_node_and_driver.node);
  auto locked_composite_node = composite_node.lock();
  ZX_ASSERT(locked_composite_node);

  auto start_result =
      bridge_->StartDriver(*locked_composite_node, composite_node_and_driver.driver);
  if (start_result.is_error()) {
    LOGF(ERROR, "Failed to start driver '%s': %s", node.name().c_str(),
         zx_status_get_string(start_result.error_value()));
    return start_result.take_error();
  }

  node.OnBind();
  return zx::ok();
}

void BindManager::ProcessPendingBindRequests() {
  ZX_ASSERT(bind_all_ongoing_);
  if (pending_bind_requests_.empty() && pending_orphan_rebind_callbacks_.empty()) {
    bind_all_ongoing_ = false;
    return;
  }

  // Consolidate the pending bind requests and orphaned nodes to prevent collisions.
  for (auto& request : pending_bind_requests_) {
    if (auto node = request.node.lock(); node) {
      orphaned_nodes_.erase(node->MakeComponentMoniker());
    }
  }

  bool have_bind_all_orphans_request = !pending_orphan_rebind_callbacks_.empty();
  size_t bind_tracker_size = have_bind_all_orphans_request
                                 ? pending_bind_requests_.size() + orphaned_nodes_.size()
                                 : pending_bind_requests_.size();

  // If there are no nodes to bind, then we'll run through all the callbacks and end the bind
  // process.
  if (have_bind_all_orphans_request && bind_tracker_size == 0) {
    for (auto& callback : pending_orphan_rebind_callbacks_) {
      fidl::Arena arena;
      callback(fidl::VectorView<fuchsia_driver_development::wire::NodeBindingInfo>(arena, 0));
    }
    pending_orphan_rebind_callbacks_.clear();
    bind_all_ongoing_ = false;
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
  for (auto& [moniker, node] : orphaned_nodes_) {
    if (std::shared_ptr locked_node = node.lock()) {
      auto orphan = orphans.CreateChild(orphans.UniqueName("orphan-"));
      orphan.RecordString("moniker", moniker);
      orphans.Record(std::move(orphan));
    }
  }

  orphans.RecordBool("bind_all_ongoing", bind_all_ongoing_);
  orphans.RecordUint("pending_bind_requests", pending_bind_requests_.size());
  orphans.RecordUint("pending_orphan_rebind_callbacks", pending_orphan_rebind_callbacks_.size());
  inspector.GetRoot().Record(std::move(orphans));

  auto dfv1_composites = inspector.GetRoot().CreateChild("dfv1_composites");
  legacy_composite_manager_.Inspect(dfv1_composites);
  inspector.GetRoot().Record(std::move(dfv1_composites));
}

std::vector<fdd::wire::CompositeInfo> BindManager::GetCompositeListInfo(
    fidl::AnyArena& arena) const {
  // TODO(fxb/119947): Add composite node specs to the list.
  return legacy_composite_manager_.GetCompositeListInfo(arena);
}

}  // namespace dfv2
