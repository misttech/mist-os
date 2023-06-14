// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_DEVICETREE_MANAGER_H_
#define LIB_DRIVER_DEVICETREE_MANAGER_H_

#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <lib/devicetree/devicetree.h>
#include <lib/driver/incoming/cpp/namespace.h>
#include <lib/stdcompat/span.h>
#include <lib/zx/result.h>

#include <unordered_map>
#include <vector>

#include "sdk/lib/driver/devicetree/node.h"
#include "sdk/lib/driver/devicetree/visitor.h"

namespace fdf_devicetree {

class Manager {
 public:
  static zx::result<Manager> CreateFromNamespace(fdf::Namespace& ns);

  // Create a new device tree manager using the given FDT blob.
  explicit Manager(std::vector<uint8_t> fdt_blob)
      : fdt_blob_(std::move(fdt_blob)),
        tree_(devicetree::ByteView{fdt_blob_.data(), fdt_blob_.size()}) {}

  // Do the initial walk of the tree.
  zx::result<> Discover();

  // Call |visitor.Visit()| for each node in the tree.
  // If |visitor.Visit()| returns something that's not `zx::ok()`, then this
  // will stop walking and return the error code.
  zx::result<> Walk(Visitor& visitor);

  // Call |BindPropertyVisitor.Visit()| for each node in the tree.
  // This will collect the node bind properties and append it to the node.
  // This needs to be called before |PublishDevices|.
  void DefaultVisit();

  // Publish the discovered devices.
  // |pbus| should be the platform bus.
  // |parent_node| is the root node of the devicetree. This will eventually be
  // used for housing the metadata nodes.
  // |mgr| is the device group manager.
  zx::result<> PublishDevices(fdf::ClientEnd<fuchsia_hardware_platform_bus::PlatformBus> pbus,
                              fidl::ClientEnd<fuchsia_driver_framework::CompositeNodeManager> mgr);

  const std::vector<std::unique_ptr<Node>>& nodes() { return nodes_publish_order_; }

 private:
  std::vector<uint8_t> fdt_blob_;
  devicetree::Devicetree tree_;

  // List of nodes, in the order that they were seen in the tree.
  std::vector<std::unique_ptr<Node>> nodes_publish_order_;
  // Nodes by phandle. Note that not every node in the tree has a phandle.
  std::unordered_map<uint32_t, Node*> nodes_by_phandle_;
  uint32_t node_id_ = 0;
};

}  // namespace fdf_devicetree

#endif  // LIB_DRIVER_DEVICETREE_MANAGER_H_
