// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_VIDEO_INPUT_REGS_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_VIDEO_INPUT_REGS_H_

#include <cstdint>

#include <hwreg/bitfields.h>
#include <hwreg/mmio.h>

namespace amlogic_display {

// There are two video input modules (VDIN) in Amlogic display engine that
// receive video from external input (e.g. BT.656) or internal input (e.g.
// internal VIU loopback), and write the data back to the DDR memory.
enum class VideoInputModuleId {
  kVideoInputModule0 = 0,
  kVideoInputModule1 = 1,
};

// VDIN0_COM_CTRL0, VDIN1_COM_CTRL0.
//
// A311D Datasheet, Section 10.2.3.42 VDIN, Pages 1086-1087, 1108-1109.
// S905D2 Datasheet, Section 7.2.3.41 VDIN, Pages 777, 801.
// S905D3 Datasheet, Section 8.2.3.42 VDIN, Pages 713-714, 736-737.
class VideoInputCommandControl : public hwreg::RegisterBase<VideoInputCommandControl, uint32_t> {
 public:
  DEF_BIT(31, bypass_mpeg_noise_reduction);
  DEF_BIT(30, mpeg_field_info);

  // Trigger a go_field (Vsync) pulse on the video input module when true is
  // written.
  DEF_BIT(29, trigger_go_field_pulse);

  // Trigger a go_line (Hsync) pulse on the video input module when true is
  // written.
  DEF_BIT(28, trigger_go_line_pulse);

  DEF_BIT(27, mpeg_go_field_input_signal_enabled);

  // Not documented for this register; for fields of the same name in other
  // registers (VD1_IF0_GEN_REG, DI_IF0_GEN_REG, etc.), `hold_lines` is the
  // number of lines to hold after go_field pulse and before the module is
  // enabled.
  DEF_FIELD(26, 20, hold_lines);

  // Whether the `go_field` pulse is delayed for the video input module.
  DEF_BIT(19, go_field_pulse_delayed);

  // Number of lines that `go_field` pulse is delayed, if
  // `go_field_pulse_delayed` is true.
  DEF_FIELD(18, 12, go_field_pulse_delay_lines);

  enum class ComponentInput : uint32_t {
    kComponentInput0 = 0b00,
    kComponentInput1 = 0b01,
    kComponentInput2 = 0b10,
  };

  // Seems unused for internal loopback mode.
  DEF_ENUM_FIELD(ComponentInput, 11, 10, component2_output_selection);
  DEF_ENUM_FIELD(ComponentInput, 9, 8, component1_output_selection);
  DEF_ENUM_FIELD(ComponentInput, 7, 6, component0_output_selection);

  // Indicates whether the video input is cropped using a window specified by
  // `VDIN0/1_WIN_H_START_END` and `VDIN0/1_WIN_V_START_END` registers.
  DEF_BIT(5, video_input_cropped);

  DEF_BIT(4, video_input_enabled);

  enum class InputSource : uint32_t {
    kNoInput = 0,
    kMpegFromDram = 1,
    kBt656 = 2,
    kReservedForComponent = 3,
    kReservedForTvDecoder = 4,
    kReservedForHdmiRx = 5,
    kDigitalVideoInput = 6,
    kVideoInputUnitInternalLoopback = 7,
    kReservedForMipiCsi2 = 8,
    kReservedForIsp = 9,
    kSecondBt656 = 10,
  };

  // If the input source doesn't equal to any value specified in `InputSource`,
  // no input is provided to the video input module.
  DEF_ENUM_FIELD(InputSource, 3, 0, input_source_selection);

  VideoInputCommandControl& SetInputSource(InputSource input_source) {
    switch (input_source) {
      case InputSource::kNoInput:
      case InputSource::kMpegFromDram:
      case InputSource::kBt656:
      case InputSource::kReservedForComponent:
      case InputSource::kReservedForTvDecoder:
      case InputSource::kReservedForHdmiRx:
      case InputSource::kDigitalVideoInput:
      case InputSource::kVideoInputUnitInternalLoopback:
      case InputSource::kReservedForMipiCsi2:
      case InputSource::kReservedForIsp:
      case InputSource::kSecondBt656:
        return set_input_source_selection(input_source);
      default:
        ZX_DEBUG_ASSERT_MSG(false, "Unsupported input source: %u",
                            static_cast<uint32_t>(input_source));
        return set_input_source_selection(InputSource::kNoInput);
    }
  }

  InputSource GetInputSource() const {
    InputSource input_source = input_source_selection();
    switch (input_source) {
      case InputSource::kNoInput:
      case InputSource::kMpegFromDram:
      case InputSource::kBt656:
      case InputSource::kReservedForComponent:
      case InputSource::kReservedForTvDecoder:
      case InputSource::kReservedForHdmiRx:
      case InputSource::kDigitalVideoInput:
      case InputSource::kVideoInputUnitInternalLoopback:
      case InputSource::kReservedForMipiCsi2:
      case InputSource::kReservedForIsp:
      case InputSource::kSecondBt656:
        return input_source;
      default:
        return InputSource::kNoInput;
    }
  }

  static constexpr uint32_t kVideoInput0RegAddr = 0x1202 * sizeof(uint32_t);
  static constexpr uint32_t kVideoInput1RegAddr = 0x1302 * sizeof(uint32_t);
  static auto Get(VideoInputModuleId video_input) {
    switch (video_input) {
      case VideoInputModuleId::kVideoInputModule0:
        return hwreg::RegisterAddr<VideoInputCommandControl>(kVideoInput0RegAddr);
      case VideoInputModuleId::kVideoInputModule1:
        return hwreg::RegisterAddr<VideoInputCommandControl>(kVideoInput1RegAddr);
      default:
        ZX_DEBUG_ASSERT_MSG(false, "Invalid video input module ID: %d",
                            static_cast<int>(video_input));
    }
  }
};

}  // namespace amlogic_display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_VIDEO_INPUT_REGS_H_
