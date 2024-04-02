// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_UI_SCENIC_LIB_DISPLAY_TESTS_MOCK_DISPLAY_COORDINATOR_H_
#define SRC_UI_SCENIC_LIB_DISPLAY_TESTS_MOCK_DISPLAY_COORDINATOR_H_

#include <fuchsia/hardware/display/cpp/fidl.h>
#include <fuchsia/hardware/display/cpp/fidl_test_base.h>
#include <fuchsia/hardware/display/types/cpp/fidl.h>
#include <lib/fidl/cpp/binding_set.h>
#include <lib/syslog/cpp/macros.h>

#include "src/lib/fsl/handles/object_info.h"
#include "src/ui/scenic/lib/display/display_coordinator_listener.h"

namespace scenic_impl {
namespace display {
namespace test {

class MockDisplayCoordinator;

struct DisplayCoordinatorObjects {
  std::shared_ptr<fuchsia::hardware::display::CoordinatorSyncPtr> interface_ptr;
  std::unique_ptr<MockDisplayCoordinator> mock;
  std::unique_ptr<DisplayCoordinatorListener> listener;
};

DisplayCoordinatorObjects CreateMockDisplayCoordinator();

class MockDisplayCoordinator : public fuchsia::hardware::display::testing::Coordinator_TestBase {
 public:
  using CheckConfigFn =
      std::function<void(bool, fuchsia::hardware::display::types::ConfigResult*,
                         std::vector<fuchsia::hardware::display::ClientCompositionOp>*)>;
  using SetDisplayColorConversionFn =
      std::function<void(fuchsia::hardware::display::types::DisplayId, std::array<float, 3>,
                         std::array<float, 9>, std::array<float, 3>)>;
  using SetMinimumRgbFn = std::function<void(uint8_t)>;
  using ImportEventFn =
      std::function<void(zx::event event, fuchsia::hardware::display::EventId event_id)>;
  using AcknowledgeVsyncFn = std::function<void(uint64_t cookie)>;
  using SetDisplayLayersFn = std::function<void(fuchsia::hardware::display::types::DisplayId,
                                                std::vector<fuchsia::hardware::display::LayerId>)>;
  using SetLayerPrimaryPositionFn = std::function<void(
      fuchsia::hardware::display::LayerId, fuchsia::hardware::display::types::Transform,
      fuchsia::hardware::display::types::Frame, fuchsia::hardware::display::types::Frame)>;

  using SetDisplayModeFn = std::function<void(fuchsia::hardware::display::types::DisplayId,
                                              fuchsia::hardware::display::Mode)>;

  explicit MockDisplayCoordinator(fuchsia::hardware::display::Info display_info)
      : display_info_(std::move(display_info)), binding_(this) {}

  void NotImplemented_(const std::string& name) final {}

  void WaitForMessage() { binding_.WaitForMessage(); }

  void Bind(zx::channel coordinator_channel, async_dispatcher_t* dispatcher = nullptr) {
    binding_.Bind(fidl::InterfaceRequest<fuchsia::hardware::display::Coordinator>(
                      std::move(coordinator_channel)),
                  dispatcher);
  }

  void set_import_event_fn(ImportEventFn fn) { import_event_fn_ = fn; }

  void ImportEvent(zx::event event, fuchsia::hardware::display::EventId event_id) override {
    ++import_event_count_;
    if (import_event_fn_) {
      import_event_fn_(std::move(event), event_id);
    }
  }

  void set_display_color_conversion_fn(SetDisplayColorConversionFn fn) {
    set_display_color_conversion_fn_ = std::move(fn);
  }

  void set_minimum_rgb_fn(SetMinimumRgbFn fn) { set_minimum_rgb_fn_ = std::move(fn); }

  void set_set_display_layers_fn(SetDisplayLayersFn fn) { set_display_layers_fn_ = std::move(fn); }

  void set_layer_primary_position_fn(SetLayerPrimaryPositionFn fn) {
    set_layer_primary_position_fn_ = std::move(fn);
  }

  void SetDisplayColorConversion(fuchsia::hardware::display::types::DisplayId display_id,
                                 std::array<float, 3> preoffsets, std::array<float, 9> coefficients,
                                 std::array<float, 3> postoffsets) override {
    ++set_display_color_conversion_count_;
    if (set_display_color_conversion_fn_) {
      set_display_color_conversion_fn_(display_id, preoffsets, coefficients, postoffsets);
    }
  }

  void SetMinimumRgb(uint8_t minimum, SetMinimumRgbCallback callback) override {
    auto result = fuchsia::hardware::display::Coordinator_SetMinimumRgb_Result::WithResponse(
        fuchsia::hardware::display::Coordinator_SetMinimumRgb_Response());
    ++set_minimum_rgb_count_;
    if (set_minimum_rgb_fn_) {
      set_minimum_rgb_fn_(minimum);
    }

    callback(std::move(result));
  }

  void CreateLayer(CreateLayerCallback callback) override {
    static uint64_t layer_id_value = 1;
    auto result = fuchsia::hardware::display::Coordinator_CreateLayer_Result::WithResponse(
        fuchsia::hardware::display::Coordinator_CreateLayer_Response({.value = layer_id_value++}));
    callback(std::move(result));
  }

  void SetDisplayLayers(fuchsia::hardware::display::types::DisplayId display_id,
                        ::std::vector<fuchsia::hardware::display::LayerId> layer_ids) override {
    ++set_display_layers_count_;
    if (set_display_layers_fn_) {
      set_display_layers_fn_(display_id, layer_ids);
    }
  }

  void ImportImage(fuchsia::hardware::display::types::ImageMetadata image_metadata,
                   fuchsia::hardware::display::BufferId buffer_id,
                   fuchsia::hardware::display::ImageId image_id,
                   ImportImageCallback callback) override {
    callback(fuchsia::hardware::display::Coordinator_ImportImage_Result::WithResponse({}));
  }

  void SetLayerPrimaryPosition(fuchsia::hardware::display::LayerId layer_id,
                               fuchsia::hardware::display::types::Transform transform,
                               fuchsia::hardware::display::types::Frame src_frame,
                               fuchsia::hardware::display::types::Frame dest_frame) override {
    ++set_layer_primary_position_count_;
    if (set_layer_primary_position_fn_) {
      set_layer_primary_position_fn_(layer_id, transform, src_frame, dest_frame);
    }
  }

  void set_check_config_fn(CheckConfigFn fn) { check_config_fn_ = std::move(fn); }

  void CheckConfig(bool discard, CheckConfigCallback callback) override {
    fuchsia::hardware::display::types::ConfigResult result =
        fuchsia::hardware::display::types::ConfigResult::OK;
    std::vector<fuchsia::hardware::display::ClientCompositionOp> ops;
    ++check_config_count_;
    if (check_config_fn_) {
      check_config_fn_(discard, &result, &ops);
    }

    callback(std::move(result), std::move(ops));
  }

  void set_acknowledge_vsync_fn(AcknowledgeVsyncFn acknowledge_vsync_fn) {
    acknowledge_vsync_fn_ = std::move(acknowledge_vsync_fn);
  }

  void AcknowledgeVsync(uint64_t cookie) override {
    ++acknowledge_vsync_count_;
    if (acknowledge_vsync_fn_) {
      acknowledge_vsync_fn_(cookie);
    }
  }

  void set_set_display_power_result(zx_status_t result) { set_display_power_result_ = result; }

  bool display_power_on() const { return display_power_on_; }

  void SetDisplayPower(fuchsia::hardware::display::types::DisplayId display_id, bool power_on,
                       SetDisplayPowerCallback callback) override {
    using SetDisplayPowerResult = fuchsia::hardware::display::Coordinator_SetDisplayPower_Result;
    auto result = set_display_power_result_;
    if (result == ZX_OK) {
      display_power_on_ = power_on;
      callback(SetDisplayPowerResult::WithResponse({}));
    } else {
      callback(SetDisplayPowerResult::WithErr(static_cast<int32_t>(result)));
    }
  }

  void SetDisplayMode(fuchsia::hardware::display::types::DisplayId display_id,
                      fuchsia::hardware::display::Mode mode) override;

  // Sends an `OnDisplayChanged()` event to the display coordinator client
  // with the default display being added.
  //
  // Must be called only after the MockDisplayCoordinator is bound to a channel.
  void SendOnDisplayChangedEvent();

  EventSender_& events() { return binding_.events(); }

  void ResetCoordinatorBinding() { binding_.Close(ZX_ERR_INTERNAL); }

  fidl::Binding<fuchsia::hardware::display::Coordinator>& binding() { return binding_; }

  const fuchsia::hardware::display::Info& display_info() const { return display_info_; }

  // Number of times each function has been called.
  uint32_t check_config_count() const { return check_config_count_; }
  uint32_t set_display_color_conversion_count() const {
    return set_display_color_conversion_count_;
  }
  uint32_t set_minimum_rgb_count() const { return set_minimum_rgb_count_; }
  uint32_t import_event_count() const { return import_event_count_; }
  uint32_t acknowledge_vsync_count() const { return acknowledge_vsync_count_; }
  uint32_t set_display_layers_count() const { return set_display_layers_count_; }
  uint32_t set_layer_primary_position_count() const { return set_layer_primary_position_count_; }
  uint32_t set_display_mode_count() const { return set_display_mode_count_; }

 private:
  CheckConfigFn check_config_fn_;
  SetDisplayColorConversionFn set_display_color_conversion_fn_;
  SetMinimumRgbFn set_minimum_rgb_fn_;
  ImportEventFn import_event_fn_;
  AcknowledgeVsyncFn acknowledge_vsync_fn_;
  SetDisplayLayersFn set_display_layers_fn_;
  SetLayerPrimaryPositionFn set_layer_primary_position_fn_;
  SetDisplayModeFn set_display_mode_fn_;

  uint32_t check_config_count_ = 0;
  uint32_t set_display_color_conversion_count_ = 0;
  uint32_t set_minimum_rgb_count_ = 0;
  uint32_t import_event_count_ = 0;
  uint32_t acknowledge_vsync_count_ = 0;
  uint32_t set_display_layers_count_ = 0;
  uint32_t set_layer_primary_position_count_ = 0;
  uint32_t set_display_mode_count_ = 0;
  zx_status_t set_display_power_result_ = ZX_OK;
  bool display_power_on_ = true;

  const fuchsia::hardware::display::Info display_info_;

  fidl::Binding<fuchsia::hardware::display::Coordinator> binding_;
};

}  // namespace test
}  // namespace display
}  // namespace scenic_impl

#endif  // SRC_UI_SCENIC_LIB_DISPLAY_TESTS_MOCK_DISPLAY_COORDINATOR_H_
