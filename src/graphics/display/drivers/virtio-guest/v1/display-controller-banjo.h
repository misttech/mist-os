// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GUEST_V1_DISPLAY_CONTROLLER_BANJO_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GUEST_V1_DISPLAY_CONTROLLER_BANJO_H_

#include <fuchsia/hardware/display/controller/cpp/banjo.h>
#include <lib/stdcompat/span.h>
#include <zircon/compiler.h>
#include <zircon/types.h>

#include <cstdint>

#include "src/graphics/display/drivers/virtio-guest/v1/display-coordinator-events-banjo.h"
#include "src/graphics/display/drivers/virtio-guest/v1/display-engine.h"

namespace virtio_display {

// Banjo <-> C++ bridge for the methods interface with the Display Coordinator.
//
// Instances are thread-safe, because Banjo does not make any threading
// guarantees.
class DisplayControllerBanjo : public ddk::DisplayControllerImplProtocol<DisplayControllerBanjo> {
 public:
  // `engine` and `coordinator_events` must not be null, and must outlive the
  // newly created instance.
  explicit DisplayControllerBanjo(DisplayEngine* engine,
                                  DisplayCoordinatorEventsBanjo* coordinator_events);

  DisplayControllerBanjo(const DisplayControllerBanjo&) = delete;
  DisplayControllerBanjo& operator=(const DisplayControllerBanjo&) = delete;

  ~DisplayControllerBanjo();

  // ddk::DisplayControllerImplProtocol
  void DisplayControllerImplSetDisplayControllerInterface(
      const display_controller_interface_protocol_t* display_controller_interface);
  void DisplayControllerImplResetDisplayControllerInterface();
  zx_status_t DisplayControllerImplImportBufferCollection(
      uint64_t banjo_driver_buffer_collection_id, zx::channel buffer_collection_token);
  zx_status_t DisplayControllerImplReleaseBufferCollection(
      uint64_t banjo_driver_buffer_collection_id);
  zx_status_t DisplayControllerImplImportImage(const image_metadata_t* banjo_image_metadata,
                                               uint64_t banjo_driver_buffer_collection_id,
                                               uint32_t index, uint64_t* out_image_handle);
  zx_status_t DisplayControllerImplImportImageForCapture(uint64_t banjo_driver_buffer_collection_id,
                                                         uint32_t index,
                                                         uint64_t* out_capture_handle);
  void DisplayControllerImplReleaseImage(uint64_t banjo_image_handle);
  config_check_result_t DisplayControllerImplCheckConfiguration(
      const display_config_t* banjo_display_configs, size_t banjo_display_configs_count,
      client_composition_opcode_t* out_client_composition_opcodes_list,
      size_t out_client_composition_opcodes_size, size_t* out_client_composition_opcodes_actual);
  void DisplayControllerImplApplyConfiguration(const display_config_t* banjo_display_configs,
                                               size_t banjo_display_configs_count,
                                               const config_stamp_t* banjo_config_stamp);
  zx_status_t DisplayControllerImplSetBufferCollectionConstraints(
      const image_buffer_usage_t* banjo_image_buffer_usage,
      uint64_t banjo_driver_buffer_collection_id);
  zx_status_t DisplayControllerImplSetDisplayPower(uint64_t banjo_display_id, bool power_on);
  bool DisplayControllerImplIsCaptureSupported();
  zx_status_t DisplayControllerImplStartCapture(uint64_t capture_handle);
  zx_status_t DisplayControllerImplReleaseCapture(uint64_t capture_handle);
  bool DisplayControllerImplIsCaptureCompleted();
  zx_status_t DisplayControllerImplSetMinimumRgb(uint8_t minimum_rgb);

  zx_status_t DdkGetProtocol(uint32_t proto_id, void* out);

 private:
  // This data member is thread-safe because it is immutable.
  DisplayEngine& engine_;

  // This data member is thread-safe because it is immutable.
  DisplayCoordinatorEventsBanjo& coordinator_events_;
};

}  // namespace virtio_display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_VIRTIO_GUEST_V1_DISPLAY_CONTROLLER_BANJO_H_
