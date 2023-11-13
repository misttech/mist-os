// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_LIB_DESIGNWARE_HDMI_DW_H_
#define SRC_GRAPHICS_DISPLAY_LIB_DESIGNWARE_HDMI_DW_H_

#include <fidl/fuchsia.hardware.hdmi/cpp/wire.h>
#include <fuchsia/hardware/i2cimpl/cpp/banjo.h>
#include <lib/mmio/mmio-buffer.h>

#include "src/graphics/display/lib/api-types-cpp/display-timing.h"
#include "src/graphics/display/lib/designware/color-param.h"

namespace hdmi_dw {

struct hdmi_param_tx {
  uint16_t vic;
  uint8_t aspect_ratio;
  uint8_t colorimetry;
  bool is4K;
};

class HdmiDw {
 public:
  explicit HdmiDw(fdf::MmioBuffer controller_mmio) : controller_mmio_(std::move(controller_mmio)) {}
  virtual ~HdmiDw() = default;

  zx_status_t InitHw();
  zx_status_t EdidTransfer(const i2c_impl_op_t* op_list, size_t op_count);

  virtual void ConfigHdmitx(const ColorParam& color_param, const display::DisplayTiming& mode,
                            const hdmi_param_tx& p);
  virtual void SetupInterrupts();
  virtual void Reset();
  virtual void SetupScdc(bool is4k);
  virtual void ResetFc();
  virtual void SetFcScramblerCtrl(bool is4k);

  void PrintRegisters();

 private:
  void WriteReg(uint32_t addr, uint8_t data) { controller_mmio_.Write8(data, addr); }
  uint8_t ReadReg(uint32_t addr) { return controller_mmio_.Read8(addr); }

  void PrintReg(std::string name, uint8_t reg);

  void ScdcWrite(uint8_t addr, uint8_t val);
  void ScdcRead(uint8_t addr, uint8_t* val);

  void ConfigCsc(const ColorParam& color_param);

  // MMIO region for the HDMI Transmitter Controller registers, as defined in
  // Section 6 "Register Descriptions" (pages 185-508) of Synopsys DesignWare
  // Cores HDMI Transmitter Controller Databook, v2.12a, dated April 2016.
  fdf::MmioBuffer controller_mmio_;
};

}  // namespace hdmi_dw

#endif  // SRC_GRAPHICS_DISPLAY_LIB_DESIGNWARE_HDMI_DW_H_
