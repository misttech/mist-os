// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BIN_DRIVER_MANAGER_COMPOSITE_NODE_SPEC_COMPOSITE_NODE_SPEC_H_
#define SRC_DEVICES_BIN_DRIVER_MANAGER_COMPOSITE_NODE_SPEC_COMPOSITE_NODE_SPEC_H_

#include <fidl/fuchsia.driver.development/cpp/wire.h>
#include <fidl/fuchsia.driver.index/cpp/fidl.h>

struct DeviceV1Wrapper;

namespace dfv2 {
class Node;
}  // namespace dfv2

using DeviceOrNode = std::variant<std::weak_ptr<DeviceV1Wrapper>, std::weak_ptr<dfv2::Node>>;

struct CompositeNodeSpecCreateInfo {
  std::string name;
  size_t size;
};

// This partially abstract class represents a composite node spec and is responsible for managing
// its state and composite node. The CompositeNodeSpec class will manage the state of its bound
// nodes while its subclasses manage the composite node under the spec. There should be a subclass
// for DFv1 and DFv2.
class CompositeNodeSpec {
 public:
  explicit CompositeNodeSpec(CompositeNodeSpecCreateInfo create_info);

  virtual ~CompositeNodeSpec() = default;

  // Called when CompositeNodeManager receives a MatchedNodeRepresentation.
  // Returns ZX_ERR_ALREADY_BOUND if it's already bound. See BindParentImpl() for return type
  // details.
  zx::result<std::optional<DeviceOrNode>> BindParent(
      fuchsia_driver_index::wire::MatchedCompositeNodeSpecInfo info,
      const DeviceOrNode& device_or_node);

  virtual fuchsia_driver_development::wire::CompositeInfo GetCompositeInfo(
      fidl::AnyArena& arena) const = 0;

  // Exposed for testing.
  const std::vector<std::optional<DeviceOrNode>>& parent_specs() const { return parent_specs_; }

 protected:
  // Subclass implementation for binding the DeviceOrNode to its composite. If the composite is not
  // yet created, the implementation is expected to create one with |info|. In DFv1, it returns
  // std::nullopt. In DFv2, if the composite is complete, it returns a pointer to the new node.
  // Otherwise, it returns std::nullopt. The lifetime of this node object is managed by
  // the parent nodes.
  virtual zx::result<std::optional<DeviceOrNode>> BindParentImpl(
      fuchsia_driver_index::wire::MatchedCompositeNodeSpecInfo info,
      const DeviceOrNode& device_or_node) = 0;

  const std::string& name() const { return name_; }

  size_t size() const { return parent_specs_.size(); }

 private:
  std::string name_;
  std::vector<std::optional<DeviceOrNode>> parent_specs_;
};

#endif  // SRC_DEVICES_BIN_DRIVER_MANAGER_COMPOSITE_NODE_SPEC_COMPOSITE_NODE_SPEC_H_
