// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BIN_DRIVER_MANAGER_COMPOSITE_NODE_SPEC_V2_H_
#define SRC_DEVICES_BIN_DRIVER_MANAGER_COMPOSITE_NODE_SPEC_V2_H_

#include "src/devices/bin/driver_manager/composite_node_spec/composite_node_spec.h"
#include "src/devices/bin/driver_manager/parent_set_collector.h"

namespace dfv2 {

class CompositeNodeSpecV2 : public CompositeNodeSpec {
 public:
  // Must only be called by Create() to ensure the objects are verified.
  CompositeNodeSpecV2(CompositeNodeSpecCreateInfo create_info, async_dispatcher_t* dispatcher,
                      NodeManager* node_manager);

  ~CompositeNodeSpecV2() override = default;

  std::optional<std::weak_ptr<dfv2::Node>> completed_composite_node() {
    return parent_set_collector_ ? parent_set_collector_->completed_composite_node() : std::nullopt;
  }

  // Exposed for testing.
  bool has_parent_set_collector_for_testing() const { return parent_set_collector_.has_value(); }

 protected:
  zx::result<std::optional<DeviceOrNode>> BindParentImpl(
      fuchsia_driver_framework::wire::CompositeParent composite_parent,
      const DeviceOrNode& device_or_node) override;

 private:
  fuchsia_driver_development::wire::CompositeNodeInfo GetCompositeInfo(
      fidl::AnyArena& arena) const override;

  void RemoveImpl(RemoveCompositeNodeCallback callback) override;

  std::optional<ParentSetCollector> parent_set_collector_;

  std::string driver_url_;

  async_dispatcher_t* const dispatcher_;
  NodeManager* node_manager_;

  // Store our composite_info for easy responses to GetCompositeInfo.
  // This is set the first time |BindParentImpl| is called.
  std::optional<fuchsia_driver_framework::CompositeInfo> composite_info_ = std::nullopt;
};

}  // namespace dfv2

#endif  // SRC_DEVICES_BIN_DRIVER_MANAGER_COMPOSITE_NODE_SPEC_V2_H_
