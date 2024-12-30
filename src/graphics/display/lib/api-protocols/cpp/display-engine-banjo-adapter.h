// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_LIB_API_PROTOCOLS_CPP_DISPLAY_ENGINE_BANJO_ADAPTER_H_
#define SRC_GRAPHICS_DISPLAY_LIB_API_PROTOCOLS_CPP_DISPLAY_ENGINE_BANJO_ADAPTER_H_

#include <fuchsia/hardware/display/controller/cpp/banjo.h>
#include <lib/driver/compat/cpp/banjo_server.h>
#include <lib/driver/compat/cpp/device_server.h>
#include <lib/stdcompat/span.h>
#include <lib/zx/channel.h>
#include <zircon/compiler.h>
#include <zircon/types.h>

#include <cstddef>
#include <cstdint>

#include "src/graphics/display/lib/api-protocols/cpp/display-engine-events-banjo.h"
#include "src/graphics/display/lib/api-protocols/cpp/display-engine-interface.h"

namespace display {

// Translates Banjo API calls to `DisplayEngineInterface` C++ method calls.
//
// This adapter implements the
// [`fuchsia.hardware.display.controller/DisplayEngine`] Banjo API.
//
// Instances are thread-safe, because Banjo does not make any threading
// guarantees.
class DisplayEngineBanjoAdapter : public ddk::DisplayEngineProtocol<DisplayEngineBanjoAdapter> {
 public:
  // `engine` receives translated Banjo calls to the
  // [`fuchsia.hardware.display.controller/DisplayEngine`] interface. It must not be
  // null, and must outlive the newly created instance.
  //
  // `engine_events` is notified when a
  // [`fuchsia.hardware.display.controller/DisplayEngineListener`] Banjo
  // interface implementation is registered or unregistered with the display
  // engine. It must not be null, and must outlive the newly created instance.
  explicit DisplayEngineBanjoAdapter(DisplayEngineInterface* engine,
                                     DisplayEngineEventsBanjo* engine_events);

  DisplayEngineBanjoAdapter(const DisplayEngineBanjoAdapter&) = delete;
  DisplayEngineBanjoAdapter& operator=(const DisplayEngineBanjoAdapter&) = delete;

  ~DisplayEngineBanjoAdapter();

  // Serves the Banjo protocol over the DFv2 compatibility server.
  //
  // The Banjo protocol implemented by this bridge is
  // [`fuchsia.hardware.display.controller/Engine`].
  compat::BanjoServer& banjo_server() { return banjo_server_; }

  // Configuration for the server returned by `banjo_server()`.
  compat::DeviceServer::BanjoConfig CreateBanjoConfig();

  // ddk::DisplayEngineProtocol
  void DisplayEngineSetListener(const display_engine_listener_protocol_t* display_engine_listener);
  void DisplayEngineUnsetListener();
  zx_status_t DisplayEngineImportBufferCollection(uint64_t banjo_driver_buffer_collection_id,
                                                  zx::channel buffer_collection_token);
  zx_status_t DisplayEngineReleaseBufferCollection(uint64_t banjo_driver_buffer_collection_id);
  zx_status_t DisplayEngineImportImage(const image_metadata_t* banjo_image_metadata,
                                       uint64_t banjo_driver_buffer_collection_id, uint32_t index,
                                       uint64_t* out_image_handle);
  zx_status_t DisplayEngineImportImageForCapture(uint64_t banjo_driver_buffer_collection_id,
                                                 uint32_t index, uint64_t* out_capture_handle);
  void DisplayEngineReleaseImage(uint64_t banjo_image_handle);
  config_check_result_t DisplayEngineCheckConfiguration(
      const display_config_t* banjo_display_configs_array, size_t banjo_display_configs_count,
      layer_composition_operations_t* out_layer_composition_operations_list,
      size_t out_layer_composition_operations_size,
      size_t* out_layer_composition_operations_actual);
  void DisplayEngineApplyConfiguration(const display_config_t* banjo_display_configs_array,
                                       size_t banjo_display_configs_count,
                                       const config_stamp_t* banjo_config_stamp);
  zx_status_t DisplayEngineSetBufferCollectionConstraints(
      const image_buffer_usage_t* banjo_image_buffer_usage,
      uint64_t banjo_driver_buffer_collection_id);
  zx_status_t DisplayEngineSetDisplayPower(uint64_t banjo_display_id, bool power_on);
  bool DisplayEngineIsCaptureSupported();
  zx_status_t DisplayEngineStartCapture(uint64_t capture_handle);
  zx_status_t DisplayEngineReleaseCapture(uint64_t capture_handle);
  zx_status_t DisplayEngineSetMinimumRgb(uint8_t minimum_rgb);

  display_engine_protocol_t GetProtocol();

 private:
  // This data member is thread-safe because it is immutable.
  DisplayEngineInterface& engine_;

  // This data member is thread-safe because it is immutable.
  DisplayEngineEventsBanjo& engine_events_;

  // Serves the Banjo protocol over the DFv2 compatibility server.
  compat::BanjoServer banjo_server_;
};

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_LIB_API_PROTOCOLS_CPP_DISPLAY_ENGINE_BANJO_ADAPTER_H_
