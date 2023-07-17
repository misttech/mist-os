// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/v2/node_removal_tracker.h"

#include <zircon/errors.h>
#include <zircon/status.h>

#include <src/devices/lib/log/log.h>

namespace dfv2 {

NodeId NodeRemovalTracker::RegisterNode(Node node) {
  nodes_[next_node_id_] = std::move(node);
  return next_node_id_++;
}

void NodeRemovalTracker::Notify(NodeId id, NodeState state) {
  auto itr = nodes_.find(id);
  if (itr == nodes_.end()) {
    LOGF(ERROR, "Tried to Notify without registering!");
    return;
  }
  itr->second.state = state;
  if (state == NodeState::kStopping) {
    CheckRemovalDone();
  }
}

void NodeRemovalTracker::CheckRemovalDone() {
  if (fully_enumerated_ == false) {
    return;
  }
  int pkg_count = 0, all_count = 0;

  // TODO(fxb/130029): Temporary hack to potentially fix an ASAN error involving
  // iterator invalidation.
  auto nodes = nodes_;
  for (const auto& [id, value] : nodes) {
    const auto& [name, node_collection, state] = value;
    if (state != NodeState::kStopping) {
      all_count++;
      LOGF(DEBUG, "NRT: %s node %s waiting on %s",
           node_collection == Collection::kPackage ? "package"
           : node_collection == Collection::kBoot  ? "boot"
                                                   : "other",
           name.c_str(),
           state == NodeState::kWaitingOnDriver     ? "driver"
           : state == NodeState::kWaitingOnChildren ? "children"
                                                    : "nothing");

      if (node_collection == Collection::kPackage) {
        pkg_count++;
      }
    }
  }
  LOGF(DEBUG, "NodeRemovalTracker: %d pkg %d all remaining", pkg_count, all_count);
  if (pkg_callback_ && pkg_count == 0) {
    pkg_callback_();
    pkg_callback_ = nullptr;
  }
  if (all_callback_ && all_count == 0) {
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
