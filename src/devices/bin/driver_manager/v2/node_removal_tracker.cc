// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/v2/node_removal_tracker.h"

#include <zircon/errors.h>
#include <zircon/status.h>

#include <src/devices/lib/log/log.h>

namespace dfv2 {

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
  if (state != NodeState::kStopping) {
    return;
  }

  if (itr->second.collection == Collection::kPackage) {
    remaining_pkg_nodes_.erase(id);
  } else {
    remaining_non_pkg_nodes_.erase(id);
  }
  CheckRemovalDone();
}

void NodeRemovalTracker::CheckRemovalDone() {
  if (fully_enumerated_ == false) {
    return;
  };

  size_t pkg_count = remaining_pkg_nodes_.size();
  size_t all_count = pkg_count + remaining_non_pkg_nodes_.size();

  // TODO(fxb/130850): Remove logs or lower their severity once the ASAN issue is resolved.
  LOGF(INFO, "NodeRemovalTracker: %zu pkg %zu all remaining", pkg_count, all_count);
  if (pkg_callback_ && pkg_count == 0) {
    LOGF(INFO, "NodeRemovalTracker: package removal completed");
    pkg_callback_();
    pkg_callback_ = nullptr;
  }
  if (all_callback_ && all_count == 0) {
    LOGF(INFO, "NodeRemovalTracker: all nodes removed");
    all_callback_();
    all_callback_ = nullptr;
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
  CheckRemovalDone();
}

}  // namespace dfv2
