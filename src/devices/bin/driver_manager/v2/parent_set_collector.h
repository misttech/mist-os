// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BIN_DRIVER_MANAGER_V2_PARENT_SET_COLLECTOR_H_
#define SRC_DEVICES_BIN_DRIVER_MANAGER_V2_PARENT_SET_COLLECTOR_H_

#include <fidl/fuchsia.driver.index/cpp/wire.h>

#include "src/devices/bin/driver_manager/v2/node.h"
#include "src/devices/lib/log/log.h"

namespace dfv2 {

// |ParentSetCollector| wraps functionality for collecting multiple parent nodes for composites.
// The parent set starts out empty and gets nodes added to it until it is complete. Once complete
// it will return a vector containing all the parent node pointers.
class ParentSetCollector {
 public:
  explicit ParentSetCollector(std::string composite_name, std::vector<std::string> parent_names,
                              uint32_t primary_index)
      : composite_name_(composite_name),
        parents_(parent_names.size()),
        parent_names_(std::move(parent_names)),
        primary_index_(primary_index) {}

  // Add a node to the parent set at the specified index.
  // Caller should check that |ContainsNode| is false for the index before calling this.
  // Only a weak_ptr of the node is stored by this class (until collection in GetIfComplete).
  zx::result<> AddNode(uint32_t index, std::weak_ptr<Node> node);

  // Remove a node at a specific index from the parent set.
  void RemoveNode(uint32_t index);

  // Check if all parents are found. If so, then create and return the composite node. If the
  // node is already created, return ZX_ERR_ALREADY_EXISTS.
  zx::result<std::shared_ptr<Node>> TryToAssemble(NodeManager* node_manager,
                                                  async_dispatcher_t* dispatcher);

  fidl::VectorView<fuchsia_driver_development::wire::CompositeParentNodeInfo> GetParentInfo(
      fidl::AnyArena& arena) const;

  const std::weak_ptr<Node>& get(uint32_t index) const { return parents_[index]; }

  uint32_t primary_index() const { return primary_index_; }

  std::optional<std::weak_ptr<dfv2::Node>> completed_composite_node() const {
    return completed_composite_node_;
  }

 private:
  std::string composite_name_;

  // Nodes are stored as weak_ptrs. Only when trying to collect the completed set are they
  // locked into shared_ptrs and validated to not be null.
  std::vector<std::weak_ptr<Node>> parents_;

  std::vector<std::string> parent_names_;

  uint32_t primary_index_;

  // Contains a weak pointer to the composite node when the parent set is assembled.
  std::optional<std::weak_ptr<dfv2::Node>> completed_composite_node_;
};

}  // namespace dfv2

#endif  // SRC_DEVICES_BIN_DRIVER_MANAGER_V2_PARENT_SET_COLLECTOR_H_
