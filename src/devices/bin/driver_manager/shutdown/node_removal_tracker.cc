// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/shutdown/node_removal_tracker.h"

#include <zircon/errors.h>
#include <zircon/status.h>

#include <src/devices/lib/log/log.h>

namespace driver_manager {

namespace {

zx::duration kRemovalTimeoutDuration = zx::sec(30);

}

NodeId NodeRemovalTracker::RegisterNode(Node node) {
  if (node.collection == Collection::kPackage) {
    remaining_pkg_nodes_.emplace(next_node_id_);
  } else {
    remaining_non_pkg_nodes_.emplace(next_node_id_);
  }
  nodes_[next_node_id_] = node;
  return next_node_id_++;
}

void NodeRemovalTracker::Notify(NodeId id, NodeState state) {
  auto itr = nodes_.find(id);
  if (itr == nodes_.end()) {
    LOGF(ERROR, "Tried to Notify without registering!");
    return;
  }
  itr->second.state = state;

  if (handle_timeout_task_.is_pending()) {
    handle_timeout_task_.Cancel();
    handle_timeout_task_.PostDelayed(dispatcher_, kRemovalTimeoutDuration);
  }

  if (state != NodeState::kStopped) {
    return;
  }

  if (itr->second.collection == Collection::kPackage) {
    remaining_pkg_nodes_.erase(id);
  } else {
    remaining_non_pkg_nodes_.erase(id);
  }
  CheckRemovalDone();
}

void NodeRemovalTracker::OnRemovalTimeout() {
  LOGF(INFO, "Removal hanging, nodes remaining: %zu pkg, %zu pkg+boot", remaining_pkg_node_count(),
       remaining_node_count());
  for (auto& [id, node] : nodes_) {
    if (node.state == NodeState::kStopped) {
      continue;
    }
    LOGF(INFO, "  Node '%s' in state %s", node.name.c_str(),
         NodeShutdownCoordinator::NodeStateAsString(node.state));
  }
  handle_timeout_task_.PostDelayed(dispatcher_, kRemovalTimeoutDuration);
}

void NodeRemovalTracker::CheckRemovalDone() {
  if (fully_enumerated_ == false) {
    return;
  };

  if (pkg_callback_ && remaining_pkg_node_count() == 0) {
    LOGF(INFO, "NodeRemovalTracker: package removal completed");
    pkg_callback_();
    pkg_callback_ = nullptr;
  }
  if (all_callback_ && remaining_node_count() == 0) {
    LOGF(INFO, "NodeRemovalTracker: all nodes removed");
    all_callback_();
    all_callback_ = nullptr;
    handle_timeout_task_.Cancel();
    nodes_.clear();
  }
}

void NodeRemovalTracker::set_pkg_callback(fit::callback<void()> callback) {
  pkg_callback_ = std::move(callback);
}
void NodeRemovalTracker::set_all_callback(fit::callback<void()> callback) {
  all_callback_ = std::move(callback);
}

void NodeRemovalTracker::FinishEnumeration() {
  fully_enumerated_ = true;
  handle_timeout_task_.PostDelayed(dispatcher_, kRemovalTimeoutDuration);
  CheckRemovalDone();
}

}  // namespace driver_manager
