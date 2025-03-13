// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_UI_SCENIC_LIB_DISPLAY_TESTS_MOCK_DISPLAY_COORDINATOR_H_
#define SRC_UI_SCENIC_LIB_DISPLAY_TESTS_MOCK_DISPLAY_COORDINATOR_H_

#include <fidl/fuchsia.hardware.display.types/cpp/fidl.h>
#include <fidl/fuchsia.hardware.display/cpp/wire_test_base.h>
#include <fidl/fuchsia.math/cpp/fidl.h>
#include <lib/fidl/cpp/binding_set.h>
#include <lib/syslog/cpp/macros.h>

namespace scenic_impl::display::test {

class MockDisplayCoordinator;

class MockDisplayCoordinator
    : public fidl::testing::WireTestBase<fuchsia_hardware_display::Coordinator> {
 public:
  using CheckConfigFn =
      std::function<void(bool, fuchsia_hardware_display_types::wire::ConfigResult*,
                         std::vector<fuchsia_hardware_display::wire::ClientCompositionOp>*)>;
  using SetDisplayColorConversionFn =
      std::function<void(fuchsia_hardware_display_types::wire::DisplayId, fidl::Array<float, 3>,
                         fidl::Array<float, 9>, fidl::Array<float, 3>)>;
  using SetMinimumRgbFn = std::function<void(uint8_t)>;
  using ImportEventFn =
      std::function<void(zx::event event, fuchsia_hardware_display::wire::EventId event_id)>;
  using AcknowledgeVsyncFn = std::function<void(uint64_t cookie)>;
  using SetDisplayLayersFn =
      std::function<void(fuchsia_hardware_display_types::wire::DisplayId,
                         fidl::VectorView<fuchsia_hardware_display::wire::LayerId>)>;
  using SetLayerPrimaryPositionFn =
      std::function<void(fuchsia_hardware_display::wire::LayerId,
                         fuchsia_hardware_display_types::wire::CoordinateTransformation,
                         fuchsia_math::wire::RectU, fuchsia_math::wire::RectU)>;

  using SetDisplayModeFn = std::function<void(fuchsia_hardware_display_types::DisplayId,
                                              fuchsia_hardware_display_types::Mode)>;

  explicit MockDisplayCoordinator(fuchsia_hardware_display::wire::Info display_info);
  ~MockDisplayCoordinator() override;

  // `fidl::testing::TestBase<fuchsia_hardware_display::Coordinator>`:
  void NotImplemented_(const std::string& name, fidl::CompleterBase& completer) final {}
  void ImportEvent(fuchsia_hardware_display::wire::CoordinatorImportEventRequest* request,
                   ImportEventCompleter::Sync& completer) override;
  void SetDisplayColorConversion(
      fuchsia_hardware_display::wire::CoordinatorSetDisplayColorConversionRequest* request,
      SetDisplayColorConversionCompleter::Sync& completer) override;
  void SetMinimumRgb(fuchsia_hardware_display::wire::CoordinatorSetMinimumRgbRequest* request,
                     SetMinimumRgbCompleter::Sync& completer) override;
  void CreateLayer(CreateLayerCompleter::Sync& completer) override;
  void SetDisplayLayers(fuchsia_hardware_display::wire::CoordinatorSetDisplayLayersRequest* request,
                        SetDisplayLayersCompleter::Sync& completer) override;
  void ImportImage(fuchsia_hardware_display::wire::CoordinatorImportImageRequest* request,
                   ImportImageCompleter::Sync& completer) override;
  void SetLayerPrimaryPosition(
      fuchsia_hardware_display::wire::CoordinatorSetLayerPrimaryPositionRequest* request,
      SetLayerPrimaryPositionCompleter::Sync& completer) override;
  void CheckConfig(fuchsia_hardware_display::wire::CoordinatorCheckConfigRequest* request,
                   CheckConfigCompleter::Sync& completer) override;
  void AcknowledgeVsync(fuchsia_hardware_display::wire::CoordinatorAcknowledgeVsyncRequest* request,
                        AcknowledgeVsyncCompleter::Sync& completer) override;
  void SetDisplayPower(fuchsia_hardware_display::wire::CoordinatorSetDisplayPowerRequest* request,
                       SetDisplayPowerCompleter::Sync& completer) override;
  void SetDisplayMode(fuchsia_hardware_display::wire::CoordinatorSetDisplayModeRequest* request,
                      SetDisplayModeCompleter::Sync& completer) override;

  void Bind(fidl::ServerEnd<fuchsia_hardware_display::Coordinator> coordinator_server,
            fidl::ClientEnd<fuchsia_hardware_display::CoordinatorListener> listener_client,
            async_dispatcher_t* dispatcher = nullptr);

  void ResetCoordinatorBinding();

  // Sends an `OnDisplayChanged()` event to the display CoordinatorListener server
  // with the default display being added.
  //
  // Must be called only after the MockDisplayCoordinator is bound to a channel.
  void SendOnDisplayChangedRequest();

  fidl::ServerBindingRef<fuchsia_hardware_display::Coordinator>& binding() {
    ZX_DEBUG_ASSERT(binding_.has_value());
    return *binding_;
  }

  fidl::WireSharedClient<fuchsia_hardware_display::CoordinatorListener>& listener() {
    return listener_;
  }

  const fuchsia_hardware_display::wire::Info& display_info() const { return display_info_; }

  void set_import_event_fn(ImportEventFn fn) { import_event_fn_ = std::move(fn); }
  void set_display_color_conversion_fn(SetDisplayColorConversionFn fn) {
    set_display_color_conversion_fn_ = std::move(fn);
  }
  void set_minimum_rgb_fn(SetMinimumRgbFn fn) { set_minimum_rgb_fn_ = std::move(fn); }
  void set_set_display_layers_fn(SetDisplayLayersFn fn) { set_display_layers_fn_ = std::move(fn); }
  void set_layer_primary_position_fn(SetLayerPrimaryPositionFn fn) {
    set_layer_primary_position_fn_ = std::move(fn);
  }
  void set_check_config_fn(CheckConfigFn fn) { check_config_fn_ = std::move(fn); }
  void set_acknowledge_vsync_fn(AcknowledgeVsyncFn acknowledge_vsync_fn) {
    acknowledge_vsync_fn_ = std::move(acknowledge_vsync_fn);
  }
  void set_set_display_power_result(zx_status_t result) { set_display_power_result_ = result; }
  bool display_power_on() const { return display_power_on_; }

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

  const fuchsia_hardware_display::wire::Info display_info_;

  std::optional<fidl::ServerBindingRef<fuchsia_hardware_display::Coordinator>> binding_;
  fidl::WireSharedClient<fuchsia_hardware_display::CoordinatorListener> listener_;
};

}  // namespace scenic_impl::display::test

#endif  // SRC_UI_SCENIC_LIB_DISPLAY_TESTS_MOCK_DISPLAY_COORDINATOR_H_
