// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BIN_DRIVER_MANAGER_V1_COMPOSITE_NODE_SPEC_V1_H_
#define SRC_DEVICES_BIN_DRIVER_MANAGER_V1_COMPOSITE_NODE_SPEC_V1_H_

#include "src/devices/bin/driver_manager/composite_node_spec/composite_manager_bridge.h"
#include "src/devices/bin/driver_manager/v1/composite_device.h"
#include "src/devices/bin/driver_manager/v1/device_manager.h"

// Wrapper struct for a fbl::RefPtr<Device>. This allows the composite node spec
// code to refer to this without any dependencies on the DFv1 code.
// TODO(fxb/106479): Move this struct and the rest of the composite node spec code
// under the namespace.
struct DeviceV1Wrapper {
  const fbl::RefPtr<Device> device;
};

namespace composite_node_specs {

// DFv1 implementation for CompositeNodeSpec. CompositeNodeSpecV1 creates and manages a
// CompositeDevice object underneath the interface.
class CompositeNodeSpecV1 : public CompositeNodeSpec {
 public:
  static zx::result<std::unique_ptr<CompositeNodeSpecV1>> Create(
      CompositeNodeSpecCreateInfo create_info,
      fuchsia_device_manager::wire::CompositeNodeSpecDescriptor spec,
      DeviceManager& device_manager);

  CompositeNodeSpecV1(CompositeNodeSpecCreateInfo create_info,
                      fbl::Array<std::unique_ptr<Metadata>> metadata,
                      DeviceManager& device_manager);

 private:
  // CompositeNodeSpec interface:
  zx::result<std::optional<DeviceOrNode>> BindParentImpl(
      fuchsia_driver_index::wire::MatchedCompositeNodeSpecInfo info,
      const DeviceOrNode& device_or_node) override;

  fuchsia_driver_development::wire::CompositeInfo GetCompositeInfo(
      fidl::AnyArena& arena) const override;

  // Should only be called when |has_composite_device_| is false.
  void SetupCompositeDevice(fuchsia_driver_index::wire::MatchedCompositeNodeSpecInfo info);

  // Used to create the CompositeDevice object. Set to empty once the object is created.
  fbl::Array<std::unique_ptr<Metadata>> metadata_;

  // Set by SetupCompositeDevice(). Used for debugging.
  std::vector<std::string> parent_names_;

  // True if a CompositeDevice object is created for the spec. Set true by SetupCompositeDevice()
  // after the first BindParentImpl() call.
  bool has_composite_device_;

  // Must outlive CompositeNodeSpecV1.
  DeviceManager& device_manager_;
};

}  // namespace composite_node_specs

#endif  // SRC_DEVICES_BIN_DRIVER_MANAGER_V1_COMPOSITE_NODE_SPEC_V1_H_
