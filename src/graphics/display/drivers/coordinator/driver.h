// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_DRIVER_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_DRIVER_H_

#include <fidl/fuchsia.hardware.display/cpp/wire.h>
#include <fuchsia/hardware/display/clamprgb/cpp/banjo.h>
#include <fuchsia/hardware/display/controller/cpp/banjo.h>

#include <ddktl/device.h>
#include <ddktl/protocol/empty-protocol.h>

namespace display {

class Controller;

// Manages the state associated with a display coordinator driver connection.
class Driver : public ddk::DisplayControllerInterfaceProtocol<Driver>,
               public ddk::DisplayCaptureInterfaceProtocol<Driver>,
               public ddk::EmptyProtocol<ZX_PROTOCOL_DISPLAY_COORDINATOR> {
 public:
  explicit Driver(Controller* controller, zx_device_t* parent);

  Driver(const Driver&) = delete;
  Driver& operator=(const Driver&) = delete;

  ~Driver();

  void DdkUnbind(ddk::UnbindTxn txn);
  void DdkRelease();
  zx_status_t Bind(std::unique_ptr<Driver>* device_ptr);

  // |DisplayControllerInterfaceProtocol|
  void DisplayControllerInterfaceOnDisplaysChanged(const added_display_args_t* displays_added,
                                                   size_t added_count,
                                                   const uint64_t* displays_removed,
                                                   size_t removed_count,
                                                   added_display_info_t* out_display_info_list,
                                                   size_t display_info_count,
                                                   size_t* display_info_actual) {}
  void DisplayControllerInterfaceOnDisplayVsync(uint64_t banjo_display_id, zx_time_t timestamp,
                                                const config_stamp_t* config_stamp) {}
  zx_status_t DisplayControllerInterfaceGetAudioFormat(
      uint64_t banjo_display_id, uint32_t fmt_idx,
      audio_types_audio_stream_format_range_t* fmt_out) {
    return ZX_OK;
  }

  // |DisplayCaptureInterfaceProtocol|
  void DisplayCaptureInterfaceOnCaptureComplete() {}

 private:
  Controller* const controller_;
  zx_device_t* parent_;

  ddk::DisplayControllerImplProtocolClient dc_;
  ddk::DisplayClampRgbImplProtocolClient dc_clamp_rgb_;
};

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_COORDINATOR_DRIVER_H_
