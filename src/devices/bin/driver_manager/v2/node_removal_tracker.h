// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BIN_DRIVER_MANAGER_V2_NODE_REMOVAL_TRACKER_H_
#define SRC_DEVICES_BIN_DRIVER_MANAGER_V2_NODE_REMOVAL_TRACKER_H_

#include <lib/fit/function.h>

#include <unordered_map>
#include <unordered_set>

#include "src/devices/bin/driver_manager/v2/node.h"

namespace dfv2 {
class NodeRemovalTracker {
 public:
  struct Node {
    std::string name;
    Collection collection;
    NodeState state;
  };

  NodeId RegisterNode(Node node);
  void Notify(NodeId id, NodeState state);

  void FinishEnumeration();

  void set_pkg_callback(fit::callback<void()> callback);
  void set_all_callback(fit::callback<void()> callback);

 private:
  void CheckRemovalDone();

  bool fully_enumerated_ = false;
  NodeId next_node_id_ = 0;

  std::unordered_set<NodeId> remaining_pkg_nodes_;
  std::unordered_set<NodeId> remaining_non_pkg_nodes_;
  std::unordered_map<NodeId, Node> nodes_;

  fit::callback<void()> pkg_callback_;
  fit::callback<void()> all_callback_;
};

}  // namespace dfv2
#endif  // SRC_DEVICES_BIN_DRIVER_MANAGER_V2_NODE_REMOVAL_TRACKER_H_
