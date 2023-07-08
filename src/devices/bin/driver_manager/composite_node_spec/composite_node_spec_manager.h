// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BIN_DRIVER_MANAGER_COMPOSITE_NODE_SPEC_COMPOSITE_NODE_SPEC_MANAGER_H_
#define SRC_DEVICES_BIN_DRIVER_MANAGER_COMPOSITE_NODE_SPEC_COMPOSITE_NODE_SPEC_MANAGER_H_

#include <fidl/fuchsia.driver.framework/cpp/wire.h>
#include <fidl/fuchsia.driver.index/cpp/fidl.h>
#include <lib/zx/result.h>

#include <unordered_map>

#include "src/devices/bin/driver_manager/composite_node_spec/composite_manager_bridge.h"

struct CompositeNodeAndDriver {
  fuchsia_driver_index::wire::MatchedDriverInfo driver;
  DeviceOrNode node;
};

// This class is responsible for managing composite node specs. It keeps track of the specs
// and their matching composite driver and nodes. CompositeNodeSpecManager is owned by a
// CompositeManagerBridge and must be outlived by it.
class CompositeNodeSpecManager {
 public:
  using CompositeNodeSpecMap = std::unordered_map<std::string, std::unique_ptr<CompositeNodeSpec>>;

  explicit CompositeNodeSpecManager(CompositeManagerBridge* bridge);

  // Adds a composite node spec to the driver index. If it's successfully added, then the
  // CompositeNodeSpecManager stores the composite node spec in a map. After that, it sends a call
  // to CompositeManagerBridge to bind all unbound devices.
  fit::result<fuchsia_driver_framework::CompositeNodeSpecError> AddSpec(
      fuchsia_driver_framework::wire::CompositeNodeSpec fidl_spec,
      std::unique_ptr<CompositeNodeSpec> spec);

  // Binds the device to the spec parents it was matched to. If |enable_multibind| is false,
  // CompositeNodeSpecManager will only bind the device to the first unbound parent. Depending
  // on the implementation, the function either return an empty vector or a list of
  // completed CompositeNodeAndDrivers.
  zx::result<std::vector<CompositeNodeAndDriver>> BindParentSpec(
      fuchsia_driver_index::wire::MatchedCompositeNodeParentInfo match_info,
      const DeviceOrNode& device_or_node, bool enable_multibind = false);
  zx::result<std::vector<CompositeNodeAndDriver>> BindParentSpec(
      fuchsia_driver_index::MatchedCompositeNodeParentInfo match_info,
      const DeviceOrNode& device_or_node, bool enable_multibind = false);

  std::vector<fuchsia_driver_development::wire::CompositeInfo> GetCompositeInfo(
      fidl::AnyArena& arena) const;

  // Exposed for testing only.
  const CompositeNodeSpecMap& specs() const { return specs_; }

 private:
  // Contains all composite node specs. This maps the name to a CompositeNodeSpec object.
  CompositeNodeSpecMap specs_;

  // The owner of CompositeNodeSpecManager. CompositeManagerBridge must outlive
  // CompositeNodeSpecManager.
  CompositeManagerBridge* bridge_;
};

#endif  // SRC_DEVICES_BIN_DRIVER_MANAGER_COMPOSITE_NODE_SPEC_COMPOSITE_NODE_SPEC_MANAGER_H_
