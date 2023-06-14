// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_DEVICETREE_NODE_H_
#define LIB_DRIVER_DEVICETREE_NODE_H_

#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <lib/devicetree/devicetree.h>

#include <string_view>
#include <utility>

namespace fdf_devicetree {

// Node represents the nodes in the device tree along with it's properties.
class Node {
 public:
  explicit Node(std::string_view name, devicetree::Properties properties, uint32_t id);

  // Add |prop| as a bind property of the device, when it is eventually published.
  void AddBindProperty(fuchsia_driver_framework::NodeProperty prop);

  // Publish this node.
  // TODO(fxbug.dev/108070): Switch to fdf::SyncClient when it's available.
  zx::result<> Publish(fdf::WireSyncClient<fuchsia_hardware_platform_bus::PlatformBus>& pbus,
                       fidl::SyncClient<fuchsia_driver_framework::CompositeNodeManager>& mgr);

  std::string_view name() const { return name_; }

  const std::unordered_map<std::string_view, devicetree::PropertyValue>& properties() const {
    return properties_;
  }

 private:
  const std::string_view name_;
  std::unordered_map<std::string_view, devicetree::PropertyValue> properties_;

  // Platform bus node.
  fuchsia_hardware_platform_bus::Node pbus_node_;

  // Properties of the nodes after they have been transformed in the device group.
  std::vector<fuchsia_driver_framework::NodeProperty> node_properties_;

  // Parent specifications.
  std::vector<fuchsia_driver_framework::ParentSpec> parents_;

  // This is a unique ID we use to match our device group with the correct
  // platform bus node. It is generated at runtime and not stable across boots.
  uint32_t id_;
};

}  // namespace fdf_devicetree

#endif  // LIB_DRIVER_DEVICETREE_NODE_H_
