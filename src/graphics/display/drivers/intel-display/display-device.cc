// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/intel-display/display-device.h"

#include <fuchsia/hardware/display/controller/c/banjo.h>
#include <lib/fit/function.h>
#include <lib/zx/result.h>
#include <lib/zx/vmo.h>
#include <zircon/assert.h>
#include <zircon/errors.h>

#include <cfloat>
#include <cinttypes>
#include <cmath>

#include "src/graphics/display/drivers/intel-display/intel-display.h"
#include "src/graphics/display/drivers/intel-display/registers-dpll.h"
#include "src/graphics/display/drivers/intel-display/registers-transcoder.h"
#include "src/graphics/display/drivers/intel-display/registers.h"
#include "src/graphics/display/drivers/intel-display/tiling.h"
#include "src/graphics/display/lib/api-types/cpp/display-id.h"
#include "src/graphics/display/lib/api-types/cpp/display-timing.h"
#include "src/graphics/display/lib/api-types/cpp/driver-config-stamp.h"
#include "src/graphics/display/lib/api-types/cpp/driver-image-id.h"

namespace intel_display {

DisplayDevice::DisplayDevice(Controller* controller, display::DisplayId id, DdiId ddi_id,
                             DdiReference ddi_reference, Type type)
    : controller_(controller),
      id_(id),
      ddi_id_(ddi_id),
      ddi_reference_(std::move(ddi_reference)),
      type_(type) {
  ZX_DEBUG_ASSERT(controller != nullptr);
}

DisplayDevice::~DisplayDevice() {
  if (pipe_) {
    controller_->pipe_manager()->ReturnPipe(pipe_);
    controller_->ResetPipePlaneBuffers(pipe_->pipe_id());
  }
  if (inited_) {
    controller_->ResetDdi(ddi_id(), pipe()->connected_transcoder_id());
  }
  if (ddi_reference_) {
    ddi_reference_.reset();
  }
}

fdf::MmioBuffer* DisplayDevice::mmio_space() const { return controller_->mmio_space(); }

bool DisplayDevice::Init() {
  ddi_power_ = controller_->power()->GetDdiPowerWellRef(ddi_id_);

  Pipe* pipe = controller_->pipe_manager()->RequestPipe(*this);
  if (!pipe) {
    FDF_LOG(ERROR, "Cannot request a new pipe!");
    return false;
  }
  set_pipe(pipe);

  if (!InitDdi()) {
    return false;
  }

  inited_ = true;

  InitBacklight();

  return true;
}

bool DisplayDevice::InitWithDdiPllConfig(const DdiPllConfig& pll_config) {
  Pipe* pipe = controller_->pipe_manager()->RequestPipeFromHardwareState(*this, mmio_space());
  if (!pipe) {
    FDF_LOG(ERROR, "Failed loading pipe from register!");
    return false;
  }
  set_pipe(pipe);
  return true;
}

void DisplayDevice::InitBacklight() {
  if (HasBacklight() && InitBacklightHw()) {
    std::ignore = SetBacklightState(true, 1.0);
  }
}

bool DisplayDevice::Resume() {
  if (pipe_) {
    if (!DdiModeset(info_)) {
      return false;
    }
    controller_->interrupts()->EnablePipeInterrupts(pipe_->pipe_id(), /*enable=*/true);
  }
  return pipe_ != nullptr;
}

void DisplayDevice::LoadActiveMode() {
  pipe_->LoadActiveMode(&info_);
  const int32_t pixel_clock_frequency_khz =
      LoadPixelRateForTranscoderKhz(pipe_->connected_transcoder_id());
  info_.pixel_clock_frequency_hz = int64_t{pixel_clock_frequency_khz} * 1'000;
  FDF_LOG(INFO, "Active pixel clock: %" PRId64 " Hz", info_.pixel_clock_frequency_hz);
}

bool DisplayDevice::CheckNeedsModeset(const display::DisplayTiming& mode) {
  // Check the clock and the flags later
  display::DisplayTiming mode_without_clock_or_flags = mode;
  mode_without_clock_or_flags.pixel_clock_frequency_hz = info_.pixel_clock_frequency_hz;
  mode_without_clock_or_flags.hsync_polarity = info_.hsync_polarity;
  mode_without_clock_or_flags.vsync_polarity = info_.vsync_polarity;
  mode_without_clock_or_flags.pixel_repetition = info_.pixel_repetition;
  mode_without_clock_or_flags.vblank_alternates = info_.vblank_alternates;
  if (mode_without_clock_or_flags != info_) {
    // Modeset is necessary if display params other than the clock frequency differ
    FDF_LOG(DEBUG, "Modeset necessary for display params");
    return true;
  }

  // TODO(stevensd): There are still some situations where the BIOS is better at setting up
  // the display than we are. The BIOS seems to not always set the hsync/vsync polarity, so
  // don't include that in the check for already initialized displays. Once we're better at
  // initializing displays, merge the flags check back into the above memcmp.
  if (mode.fields_per_frame != info_.fields_per_frame) {
    FDF_LOG(DEBUG, "Modeset necessary for display flags");
    return true;
  }

  if (mode.pixel_clock_frequency_hz == info_.pixel_clock_frequency_hz) {
    // Modeset is necessary not necessary if all display params are the same
    return false;
  }

  // Check to see if the hardware was already configured properly. The is primarily to
  // prevent unnecessary modesetting at startup. The extra work this adds to regular
  // modesetting is negligible.
  DdiPllConfig desired_pll_config =
      ComputeDdiPllConfig(/*pixel_clock_khz=*/
                          static_cast<int32_t>(info_.pixel_clock_frequency_hz / 1'000));
  ZX_DEBUG_ASSERT_MSG(desired_pll_config.IsEmpty(),
                      "CheckDisplayMode() should have rejected unattainable pixel rates");
  return !controller()->dpll_manager()->DdiPllMatchesConfig(ddi_id(), desired_pll_config);
}

void DisplayDevice::ApplyConfiguration(const display_config_t* banjo_display_config,
                                       display::DriverConfigStamp config_stamp) {
  ZX_ASSERT(banjo_display_config);

  const display::DisplayTiming display_timing_params =
      display::ToDisplayTiming(banjo_display_config->mode);
  if (CheckNeedsModeset(display_timing_params)) {
    if (pipe_) {
      // TODO(https://fxbug.dev/42067272): When ApplyConfiguration() early returns on the
      // following error conditions, we should reset the DDI, pipe and transcoder
      // so that they can be possibly reused.
      if (!DdiModeset(display_timing_params)) {
        FDF_LOG(ERROR, "Display %lu: Modeset failed; ApplyConfiguration() aborted.", id().value());
        return;
      }

      if (!PipeConfigPreamble(display_timing_params, pipe_->pipe_id(),
                              pipe_->connected_transcoder_id())) {
        FDF_LOG(ERROR,
                "Display %lu: Transcoder configuration failed before pipe setup; "
                "ApplyConfiguration() aborted.",
                id().value());
        return;
      }
      pipe_->ApplyModeConfig(display_timing_params);
      if (!PipeConfigEpilogue(display_timing_params, pipe_->pipe_id(),
                              pipe_->connected_transcoder_id())) {
        FDF_LOG(ERROR,
                "Display %lu: Transcoder configuration failed after pipe setup; "
                "ApplyConfiguration() aborted.",
                id().value());
        return;
      }
    }
    info_ = display_timing_params;
  }

  if (pipe_) {
    pipe_->ApplyConfiguration(
        banjo_display_config, config_stamp,
        [controller = controller_](const image_metadata_t& image_metadata, uint64_t image_handle,
                                   uint32_t rotation) -> const GttRegion& {
          return controller->SetupGttImage(image_metadata, image_handle, rotation);
        },
        [controller = controller_](uint64_t image_handle) -> PixelFormatAndModifier {
          const display::DriverImageId image_id = display::ToDriverImageId(image_handle);
          return controller->GetImportedImagePixelFormat(image_id);
        });
  }
}

}  // namespace intel_display
