// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BIN_DRIVER_MANAGER_COMPOSITE_NODE_SPEC_COMPOSITE_MANAGER_BRIDGE_H_
#define SRC_DEVICES_BIN_DRIVER_MANAGER_COMPOSITE_NODE_SPEC_COMPOSITE_MANAGER_BRIDGE_H_

#include "src/devices/bin/driver_manager/composite_node_spec/composite_node_spec.h"

using AddToIndexCallback =
    fit::callback<void(zx::result<fuchsia_driver_index::DriverIndexAddCompositeNodeSpecResponse>)>;

using RequestRebindCallback = fit::callback<void(
    zx::result<fuchsia_driver_index::DriverIndexRebindCompositeNodeSpecResponse>)>;

// Bridge class for the composite device handling in DFv1 and DFv2.
// Implemented by the Coordinator in DFv1 and DriverRunner in DFv2.
class CompositeManagerBridge {
 public:
  virtual ~CompositeManagerBridge() = default;

  // Match and bind all unbound nodes. Called by the CompositeNodeManager
  // after a composite node spec is matched to a composite driver.
  virtual void BindNodesForCompositeNodeSpec() = 0;

  virtual void AddSpecToDriverIndex(fuchsia_driver_framework::wire::CompositeNodeSpec spec,
                                    AddToIndexCallback callback) = 0;

  virtual void RequestRebindFromDriverIndex(std::string spec,
                                            std::optional<std::string> driver_url_suffix,
                                            RequestRebindCallback callback) {
    callback(zx::error(ZX_ERR_NOT_SUPPORTED));
  }
};

#endif  // SRC_DEVICES_BIN_DRIVER_MANAGER_COMPOSITE_NODE_SPEC_COMPOSITE_MANAGER_BRIDGE_H_
