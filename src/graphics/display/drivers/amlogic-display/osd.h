// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_OSD_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_OSD_H_

#include <fuchsia/hardware/display/controller/cpp/banjo.h>
#include <lib/device-protocol/pdev-fidl.h>
#include <lib/inspect/cpp/inspect.h>
#include <lib/mmio/mmio.h>
#include <lib/zircon-internal/thread_annotations.h>
#include <lib/zx/bti.h>
#include <lib/zx/handle.h>
#include <lib/zx/interrupt.h>
#include <lib/zx/pmt.h>
#include <threads.h>
#include <zircon/compiler.h>
#include <zircon/types.h>

#include <cstdint>
#include <optional>

#include <fbl/auto_lock.h>
#include <fbl/condition_variable.h>
#include <fbl/mutex.h>

#include "src/graphics/display/drivers/amlogic-display/common.h"
#include "src/graphics/display/drivers/amlogic-display/rdma.h"
#include "src/graphics/display/lib/api-types-cpp/config-stamp.h"

namespace amlogic_display {

class Osd {
 public:
  static zx::result<std::unique_ptr<Osd>> Create(ddk::PDevFidl* pdev, uint32_t fb_width,
                                                 uint32_t fb_height, uint32_t display_width,
                                                 uint32_t display_height,
                                                 inspect::Node* parent_node);

  Osd(Osd& other) = delete;

  void HwInit();

  // Disable the OSD and set the latest stamp to |config_stamp|.
  // If the driver disables (pauses) the OSD because the client sets an empty
  // config, the |config_stamp| should be the client-provided stamp; otherwise
  // it should use the invalid stamp value indicating that the OSD has been
  // invalidated.
  void Disable(display::ConfigStamp config_stamp = display::kInvalidConfigStamp);
  void Enable();

  // Schedules the given |config| to be applied by the RDMA engine when the next VSYNC interrupt
  // occurs.
  void FlipOnVsync(uint8_t idx, const display_config_t* config, display::ConfigStamp config_stamp);

  // Returns the image handle that was most recently processed by the RDMA engine. If RDMA is
  // determined to be in progress and incomplete, then the previously applied image is returned. If
  // RDMA is determined to be complete at the time of a call, then the RDMA engine registers are
  // updated accordingly.
  //
  // This function is used by the vsync thread to determine the latest applied config.
  display::ConfigStamp GetLastConfigStampApplied();

  void Dump();
  void Release();

  // This function converts a float into Signed fixed point 3.10 format
  // [12][11:10][9:0] = [sign][integer][fraction]
  static uint32_t FloatToFixed3_10(float f);
  // This function converts a float into Signed fixed point 2.10 format
  // [11][10][9:0] = [sign][integer][fraction]
  static uint32_t FloatToFixed2_10(float f);

  void SetMinimumRgb(uint8_t minimum_rgb);

 private:
  Osd(uint32_t fb_width, uint32_t fb_height, uint32_t display_width, uint32_t display_height,
      inspect::Node* inspect_node, std::optional<fdf::MmioBuffer> vpu_mmio,
      std::unique_ptr<RdmaEngine> rdma);
  void DefaultSetup();
  // this function sets up scaling based on framebuffer and actual display
  // dimensions. The scaling IP and registers and undocumented.
  void EnableScaling(bool enable);

  zx_status_t ConfigAfbc();
  void SetColorCorrection(uint32_t rdma_table_idx, const display_config_t* config);

  void DumpNonRdmaRegisters();

  std::optional<fdf::MmioBuffer> vpu_mmio_;

  // Framebuffer dimension
  uint32_t fb_width_;
  uint32_t fb_height_;
  // Actual display dimension
  uint32_t display_width_;
  uint32_t display_height_;

  // All current metrics have been moved to RdmaEngine.
  // inspect::Node* inspect_node_;
  std::unique_ptr<RdmaEngine> rdma_;
  thrd_t rdma_irq_thread_;
};

}  // namespace amlogic_display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_OSD_H_
