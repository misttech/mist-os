// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BIN_DRIVER_MANAGER_BIND_BIND_NODE_SET_H_
#define SRC_DEVICES_BIN_DRIVER_MANAGER_BIND_BIND_NODE_SET_H_

#include <unordered_map>

#include "src/devices/bin/driver_manager/node.h"

namespace driver_manager {

// This class keeps track of all the nodes available for binding. Its purpose is to prevent the set
// of nodes from being modified during an ongoing bind process, which is the cause of most async
// bind errors. During an ongoing bind process, it'll keep track of any new changes to the orphaned
// and multibind nodes, and then apply the changes once the process is complete.
class BindNodeSet {
 public:
  // Starts the next bind process. If there's already an ongoing bind, complete it and prepare
  // the node sets for the next bind process.
  void StartNextBindProcess();

  // Complete the bind process and set |is_bind_ongoing_| to false. Must only be called when
  // |is_bind_ongoing_| is true.
  void EndBindProcess();

  void AddOrphanedNode(Node& node);

  // If available, remove the node with the matching |node_moniker| from |orphaned_nodes_|.
  void RemoveOrphanedNode(std::string node_moniker);

  // Add |node| to |multibind_nodes_|. Remove it from |orphaned_nodes_| if it exists.
  void AddOrMoveMultibindNode(Node& node);
  bool MultibindContains(std::string node_moniker) const;

  // Functions to return a copy of |orphaned_nodes_| and |multibind_nodes_|. We return a copy, not
  // const reference to prevent iterator invalidating errors.
  std::unordered_map<std::string, std::weak_ptr<Node>> CurrentOrphanedNodes() const {
    return orphaned_nodes_;
  }
  std::unordered_map<std::string, std::weak_ptr<Node>> CurrentMultibindNodes() const {
    return multibind_nodes_;
  }

  size_t NumOfOrphanedNodes() const { return orphaned_nodes_.size(); }
  size_t NumOfAvailableNodes() const { return orphaned_nodes_.size() + multibind_nodes_.size(); }

  bool is_bind_ongoing() const { return is_bind_ongoing_; }

 private:
  // Completes the current bind process by applying all the new changes to |orphaned_nodes_| and
  // |multibind_nodes_|. Must only be called when |is_bind_ongoing_| is true.
  void CompleteOngoingBind();

  // Orphaned nodes are nodes that have failed to bind to a driver, either
  // because no matching driver could be found, or because the matching driver
  // failed to start. Maps the node's component moniker to its weak pointer. Should be mutually
  // exclusive to |multibind_nodes_|.
  std::unordered_map<std::string, std::weak_ptr<Node>> orphaned_nodes_;

  // A list of nodes that can multibind to composites. In DFv1, a node can parent multiple composite
  // nodes. To follow that same behavior, we store the parents in a map to bind them
  // to other composites. A node is added to this set if it's matched to a composite's parent and
  // contains the composite multibindable flag. Should be mutually exclusive to |orphaned_nodes_|.
  std::unordered_map<std::string, std::weak_ptr<Node>> multibind_nodes_;

  // Sets that contain the new changes to |orphaned_nodes_| and |multibind_nodes_|. When
  // CompleteOngoingBind() is called, the changes transferred over to them.
  std::unordered_map<std::string, std::weak_ptr<Node>> new_orphaned_nodes_;
  std::unordered_map<std::string, std::weak_ptr<Node>> new_multibind_nodes_;

  // True when a bind process is ongoing. Set to true by StartNextBindProcess() and false by
  // EndBindProcess().
  bool is_bind_ongoing_ = false;
};

}  // namespace driver_manager

#endif  // SRC_DEVICES_BIN_DRIVER_MANAGER_BIND_BIND_NODE_SET_H_
