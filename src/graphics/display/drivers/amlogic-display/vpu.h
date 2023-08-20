// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_VPU_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_VPU_H_

#include <lib/device-protocol/pdev-fidl.h>
#include <lib/mmio/mmio.h>
#include <lib/zircon-internal/thread_annotations.h>
#include <zircon/compiler.h>

#include <optional>

#include <fbl/auto_lock.h>
#include <fbl/mutex.h>

#include "src/graphics/display/drivers/amlogic-display/common.h"
#include "src/graphics/display/drivers/amlogic-display/vout.h"
#include "src/graphics/display/drivers/amlogic-display/vpu-regs.h"

namespace amlogic_display {

// Mode of color space conversion from the internal Video Input Unit (VIU) to
// the Video output module (Vout) by the Video Post Processor (VPP).
enum class ColorSpaceConversionMode {
  kRgbInternalRgbOut,
  kRgbInternalYuvOut,
};

// TODO(fxbug.dev/126195): `Vpu` currently contains multiple relatively
// independent units of the greater Video Processing Unit (VPU) including power
// control, AFBC engine control, Video Post-processing matrices and capture
// engine. These functional units should be split into different classes.
class Vpu {
 public:
  Vpu() = default;
  ~Vpu() = default;

  // Disallows copying and moving.
  Vpu(const Vpu&) = delete;
  Vpu(Vpu&&) = delete;
  Vpu& operator=(const Vpu&) = delete;
  Vpu& operator=(Vpu&&) = delete;

  // Initialization work that is not suitable for the constructor.
  // Must be called exactly once and before any other method.
  zx_status_t Init(ddk::PDevFidl& pdev);

  // Powers on the hardware.
  void PowerOn();

  // Powers off the hardware.
  void PowerOff();

  // Sets up video post processor (VPP) output interfaces.
  // The hardware must be powered on.
  void SetupPostProcessorOutputInterface();

  // Sets up video post processor (VPP) color conversion matrices.
  // The hardware must be powered on.
  void SetupPostProcessorColorConversion(ColorSpaceConversionMode mode);

  // Claims the ownership of the driver by changing the hardware state.
  // The hardware state change reflects that the driver owns and drives
  // the hardware and it can survive driver reloads.
  //
  // Returns true iff the hardware was owned by a different driver.
  bool CheckAndClaimHardwareOwnership();

  // Powers on/off AFBC Engine.
  // The main power of the Video Processing Unit must be powered on.
  void AfbcPower(bool power_on);

  zx_status_t CaptureInit(uint8_t canvas_idx, uint32_t height, uint32_t stride);
  zx_status_t CaptureStart();
  zx_status_t CaptureDone();
  void CapturePrintRegisters();

  CaptureState GetCaptureState() {
    fbl::AutoLock lock(&capture_mutex_);
    return capture_state_;
  }

 private:
  // This function configures the VPU-related clocks. It contains undocumented registers
  // and/or clock initialization sequences
  void ConfigureClock();

  std::optional<fdf::MmioBuffer> vpu_mmio_;
  std::optional<fdf::MmioBuffer> hhi_mmio_;
  std::optional<fdf::MmioBuffer> aobus_mmio_;
  std::optional<fdf::MmioBuffer> reset_mmio_;

  bool initialized_ = false;

  uint32_t first_time_load_ = false;

  fbl::Mutex capture_mutex_;
  CaptureState capture_state_ TA_GUARDED(capture_mutex_);
};
}  // namespace amlogic_display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_VPU_H_
