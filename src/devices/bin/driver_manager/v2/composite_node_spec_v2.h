// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BIN_DRIVER_MANAGER_V2_COMPOSITE_NODE_SPEC_V2_H_
#define SRC_DEVICES_BIN_DRIVER_MANAGER_V2_COMPOSITE_NODE_SPEC_V2_H_

#include "src/devices/bin/driver_manager/composite_node_spec/composite_node_spec.h"
#include "src/devices/bin/driver_manager/v2/parent_set_collector.h"

namespace dfv2 {

class CompositeNodeSpecV2;

// Serves the fuchsia_device::Controller protocol for a composite node. Only the Rebind function
// is implemented. The implementation for it is very minimal and only supports Rebinding to the
// same driver, or whatever driver the caller asks for specifically. It does not go through the
// driver index matching process.
// TODO(fxb/124976): Replace with a long term solution for Rebind()
class CompositeNodeDeviceController : public fidl::WireServer<fuchsia_device::Controller> {
 public:
  // Ensure that node_spec outlives this.
  CompositeNodeDeviceController(CompositeNodeSpecV2& node_spec, async_dispatcher_t* dispatcher)
      : node_spec_(node_spec), dispatcher_(dispatcher) {}

  void Serve(fidl::ServerEnd<fuchsia_device::Controller> server_end);

 private:
  // fidl::WireServer<fuchsia_device::Controller>
  void ConnectToDeviceFidl(ConnectToDeviceFidlRequestView request,
                           ConnectToDeviceFidlCompleter::Sync& completer) override {
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }
  void Bind(BindRequestView request, BindCompleter::Sync& completer) override {
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }
  void Rebind(RebindRequestView request, RebindCompleter::Sync& completer) override;
  void UnbindChildren(UnbindChildrenCompleter::Sync& completer) override {
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }
  void ScheduleUnbind(ScheduleUnbindCompleter::Sync& completer) override {
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }
  void GetTopologicalPath(GetTopologicalPathCompleter::Sync& completer) override {
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }
  void GetMinDriverLogSeverity(GetMinDriverLogSeverityCompleter::Sync& completer) override {
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }
  void GetCurrentPerformanceState(GetCurrentPerformanceStateCompleter::Sync& completer) override {
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }
  void SetMinDriverLogSeverity(SetMinDriverLogSeverityRequestView request,
                               SetMinDriverLogSeverityCompleter::Sync& completer) override {
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }
  void SetPerformanceState(SetPerformanceStateRequestView request,
                           SetPerformanceStateCompleter::Sync& completer) override {
    completer.Close(ZX_ERR_NOT_SUPPORTED);
  }

  CompositeNodeSpecV2& node_spec_;
  async_dispatcher_t* const dispatcher_;
  fidl::ServerBindingGroup<fuchsia_device::Controller> device_controller_bindings_;
};

class CompositeNodeSpecV2 : public CompositeNodeSpec {
 public:
  // Must only be called by Create() to ensure the objects are verified.
  CompositeNodeSpecV2(CompositeNodeSpecCreateInfo create_info, async_dispatcher_t* dispatcher,
                      NodeManager* node_manager);

  ~CompositeNodeSpecV2() override = default;

  std::optional<std::weak_ptr<dfv2::Node>> completed_composite_node() {
    return completed_composite_node_;
  }

 protected:
  zx::result<std::optional<DeviceOrNode>> BindParentImpl(
      fuchsia_driver_index::wire::MatchedCompositeNodeSpecInfo info,
      const DeviceOrNode& device_or_node) override;

 private:
  std::shared_ptr<CompositeNodeDeviceController> device_controller() { return device_controller_; }

  std::optional<ParentSetCollector> parent_set_collector_;
  async_dispatcher_t* const dispatcher_;
  std::shared_ptr<CompositeNodeDeviceController> device_controller_;
  NodeManager* node_manager_;
  // When the composite node spec is completed, we store the newly created composite node here.
  std::optional<std::weak_ptr<dfv2::Node>> completed_composite_node_;
};

}  // namespace dfv2

#endif  // SRC_DEVICES_BIN_DRIVER_MANAGER_V2_COMPOSITE_NODE_SPEC_V2_H_
