// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/ui/scenic/lib/display/tests/mock_display_coordinator.h"

#include <fidl/fuchsia.hardware.display.types/cpp/fidl.h>
#include <fidl/fuchsia.hardware.display/cpp/fidl.h>
#include <fidl/fuchsia.math/cpp/fidl.h>
#include <lib/async/default.h>
#include <lib/syslog/cpp/macros.h>

#include <algorithm>

namespace scenic_impl {
namespace display {
namespace test {

MockDisplayCoordinator::MockDisplayCoordinator(fuchsia_hardware_display::wire::Info display_info)
    : display_info_(display_info) {}

MockDisplayCoordinator::~MockDisplayCoordinator() = default;

void MockDisplayCoordinator::Bind(
    fidl::ServerEnd<fuchsia_hardware_display::Coordinator> coordinator_server,
    fidl::ClientEnd<fuchsia_hardware_display::CoordinatorListener> listener_client,
    async_dispatcher_t* dispatcher) {
  if (dispatcher == nullptr) {
    dispatcher = async_get_default_dispatcher();
  }
  binding_ = fidl::BindServer(dispatcher, std::move(coordinator_server), this);
  listener_.Bind(std::move(listener_client), dispatcher);
}

void MockDisplayCoordinator::ResetCoordinatorBinding() {
  if (binding_.has_value()) {
    binding_->Close(ZX_ERR_INTERNAL);
    binding_ = std::nullopt;
  }
  listener_ = {};
}

void MockDisplayCoordinator::ImportEvent(
    fuchsia_hardware_display::wire::CoordinatorImportEventRequest* request,
    ImportEventCompleter::Sync& completer) {
  ++import_event_count_;
  if (import_event_fn_) {
    import_event_fn_(std::move(request->event), request->id);
  }
}

void MockDisplayCoordinator::SetDisplayColorConversion(
    fuchsia_hardware_display::wire::CoordinatorSetDisplayColorConversionRequest* request,
    SetDisplayColorConversionCompleter::Sync& completer) {
  ++set_display_color_conversion_count_;
  if (set_display_color_conversion_fn_) {
    set_display_color_conversion_fn_(request->display_id, request->preoffsets,
                                     request->coefficients, request->postoffsets);
  }
}

void MockDisplayCoordinator::SetMinimumRgb(
    fuchsia_hardware_display::wire::CoordinatorSetMinimumRgbRequest* request,
    SetMinimumRgbCompleter::Sync& completer) {
  ++set_minimum_rgb_count_;
  if (set_minimum_rgb_fn_) {
    set_minimum_rgb_fn_(request->minimum_rgb);
  }

  completer.Reply(fit::ok());
}

void MockDisplayCoordinator::CreateLayer(CreateLayerCompleter::Sync& completer) {
  static uint64_t layer_id_value = 1;
  fuchsia_hardware_display::wire::CoordinatorCreateLayerResponse response{
      {.value = layer_id_value++},
  };
  completer.Reply(fit::ok(&response));
}

void MockDisplayCoordinator::SetDisplayLayers(
    fuchsia_hardware_display::wire::CoordinatorSetDisplayLayersRequest* request,
    SetDisplayLayersCompleter::Sync& completer) {
  ++set_display_layers_count_;
  if (set_display_layers_fn_) {
    set_display_layers_fn_(request->display_id, request->layer_ids);
  }
}

void MockDisplayCoordinator::ImportImage(
    fuchsia_hardware_display::wire::CoordinatorImportImageRequest* request,
    ImportImageCompleter::Sync& completer) {
  completer.Reply(fit::ok());
}

void MockDisplayCoordinator::SetLayerPrimaryPosition(
    fuchsia_hardware_display::wire::CoordinatorSetLayerPrimaryPositionRequest* request,
    SetLayerPrimaryPositionCompleter::Sync& completer) {
  ++set_layer_primary_position_count_;
  if (set_layer_primary_position_fn_) {
    set_layer_primary_position_fn_(request->layer_id, request->image_source_transformation,
                                   request->image_source, request->display_destination);
  }
}

void MockDisplayCoordinator::CheckConfig(
    fuchsia_hardware_display::wire::CoordinatorCheckConfigRequest* request,
    CheckConfigCompleter::Sync& completer) {
  fuchsia_hardware_display_types::ConfigResult result =
      fuchsia_hardware_display_types::ConfigResult::kOk;
  std::vector<fuchsia_hardware_display::wire::ClientCompositionOp> ops;
  ++check_config_count_;
  if (check_config_fn_) {
    check_config_fn_(request->discard, &result, &ops);
  }

  completer.Reply(
      result,
      fidl::VectorView<fuchsia_hardware_display::wire::ClientCompositionOp>::FromExternal(ops));
}

void MockDisplayCoordinator::AcknowledgeVsync(
    fuchsia_hardware_display::wire::CoordinatorAcknowledgeVsyncRequest* request,
    AcknowledgeVsyncCompleter::Sync& completer) {
  ++acknowledge_vsync_count_;
  if (acknowledge_vsync_fn_) {
    acknowledge_vsync_fn_(request->cookie);
  }
}

void MockDisplayCoordinator::SetDisplayPower(
    fuchsia_hardware_display::wire::CoordinatorSetDisplayPowerRequest* request,
    SetDisplayPowerCompleter::Sync& completer) {
  if (set_display_power_result_ == ZX_OK) {
    display_power_on_ = request->power_on;
    completer.Reply(fit::ok());
  } else {
    completer.Reply(fit::error(set_display_power_result_));
  }
}

void MockDisplayCoordinator::SetDisplayMode(
    fuchsia_hardware_display::wire::CoordinatorSetDisplayModeRequest* request,
    SetDisplayModeCompleter::Sync& completer) {
  auto request_mode = fidl::ToNatural(request->mode);
  for (auto& mode : display_info_.modes) {
    if (fidl::ToNatural(mode) == request_mode) {
      return;
    }
  }
  FX_CHECK(false) << "Failed to set display mode";
}

void MockDisplayCoordinator::SendOnDisplayChangedRequest() {
  FX_CHECK(binding_.has_value());
  fidl::OneWayStatus result = listener().sync()->OnDisplaysChanged(
      fidl::VectorView<fuchsia_hardware_display::wire::Info>::FromExternal(
          const_cast<fuchsia_hardware_display::wire::Info*>(&display_info_), 1),
      {});
  FX_CHECK(result.ok());
}

}  // namespace test
}  // namespace display
}  // namespace scenic_impl
