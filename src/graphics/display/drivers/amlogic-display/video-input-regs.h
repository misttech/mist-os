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

  // Values for the `input_source_selection` field.
  enum class InputSource : uint32_t {
    kNoInput = 0,
    kMpegFromDram = 1,
    kBt656 = 2,
    kReservedForComponent = 3,
    kReservedForTvDecoder = 4,
    kReservedForHdmiRx = 5,
    kDigitalVideoInput = 6,

    // Source selected by `WritebackMuxControl`.
    //
    // This source is documented as "Internal loopback from VIU" on A311D,
    // S905D2, and T931. Experiments on VIM3 (A311D), Astro (S905D2) and
    // Sherlock (T931) confirmed that the behavior actually matches S905D3.
    kWritebackMux0 = 7,

    kReservedForMipiCsi2 = 8,

    // Source selected by `WritebackMuxControl`.
    //
    // This source is documented as "Reserved (ISP)" on A311D, S905D2, and T931.
    // Experiments on VIM3 (A311D), Astro (S905D2) and Sherlock (T931) confirmed
    // that the behavior actually matches S905D3.
    kWritebackMux1 = 9,

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
      case InputSource::kWritebackMux0:
      case InputSource::kReservedForMipiCsi2:
      case InputSource::kWritebackMux1:
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
      case InputSource::kWritebackMux0:
      case InputSource::kReservedForMipiCsi2:
      case InputSource::kWritebackMux1:
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

// VDIN0_COM_STATUS0, VDIN1_COM_STATUS0
//
// This register is read-only.
//
// A311D Datasheet, Section 10.2.3.42 VDIN, Pages 1087, 1109
// S905D2 Datasheet, Section 7.2.3.41 VDIN, Pages 778, 802
// S905D3 Datasheet, Section 8.2.3.41 VDIN, Pages 714, 737
class VideoInputCommandStatus0 : public hwreg::RegisterBase<VideoInputCommandStatus0, uint32_t> {
 public:
  // Bits 17-3` are defined differently for VDIN0_COM_STATUS0 and
  // VDIN0_COM_STATUS0, in all the datasheets mentioned above.
  //
  // VDIN0_COM_STATUS0 uses bit 17 as `vid_wr_pending_ddr_wrrsp`, bit 16
  // as `curr_pic_sec`, and bit 15 as `curr_pic_sec_sav`, bits 14-3 as
  // `lfifo_buf_cnt`.
  //
  // VDIN1_COM_STATUS0 uses bits 12-3 as `lfifo_buf_cnt` and bits 17-13
  // reserved.
  //
  // Since these fields are not currently used by the driver, we are omitting
  // these fields as reserved. Future drivers may need to fork the register
  // definitions or create helper functions to access the fields.

  // Indicates that the write of raw pixels from input source to RAM is done.
  //
  // Cleared by `clear_direct_write_done` bit in `VDIN0/1_WR_CTRL` register.
  DEF_BIT(2, direct_write_done);

  // Indicates that the write of noise-reduced (NR) pixels from input source to
  // RAM is done.
  //
  // Cleared by `clear_noise_reduced_write_done` bit in `VDIN0/1_WR_CTRL`
  // register.
  DEF_BIT(1, noise_reduced_write_done);

  // Current field for interlaced input.
  //
  // For interlaced inputs, 0 means top field, 1 means bottom field.
  // This is not documented in Amlogic datasheets but appears in drivers.
  //
  // Unused for progressive inputs.
  DEF_BIT(0, current_field);

  static auto Get(VideoInputModuleId video_input) {
    switch (video_input) {
      case VideoInputModuleId::kVideoInputModule0:
        static constexpr uint32_t kVideoInput0RegAddr = 0x1205 * sizeof(uint32_t);
        return hwreg::RegisterAddr<VideoInputCommandStatus0>(kVideoInput0RegAddr);
      case VideoInputModuleId::kVideoInputModule1:
        static constexpr uint32_t kVideoInput1RegAddr = 0x1305 * sizeof(uint32_t);
        return hwreg::RegisterAddr<VideoInputCommandStatus0>(kVideoInput1RegAddr);
      default:
        ZX_DEBUG_ASSERT_MSG(false, "Invalid video input module ID: %d",
                            static_cast<int>(video_input));
    }
  }
};

// There are four control registers (ASFIFO_CTRL0/1/2/3) for video input
// channels (VDI) channel 1 to channel 9. Layout of async FIFO configuration
// fields are the same for each async FIFO.
//
// Currently this driver only uses VDI channel 6; control fields / registers
// for all the other VDI channels are left reserved.

// VDIN0_ASFIFO_CTRL3, VDIN1_ASFIFO_CTRL3
//
// Each video input module has multiple video input channels (VDIs) and async
// FIFOs (ASFIFO). They are hard-wired for each type of video input.
// For example, VIU loopback output always use video input channel VDI6.
//
// A311D Datasheet, Section 10.2.3.42 VDIN, Pages 1088-1090, 1106, 1110-1112,
// 1128.
// S905D2 Datasheet, Section 7.2.3.41 VDIN, Pages 779-781, 798-799, 802-805,
// 822.
// S905D3 Datasheet, Section 8.2.3.42 VDIN, Pages 715-717, 733, 738-740, 755.
class VideoInputAsyncFifoControl3
    : public hwreg::RegisterBase<VideoInputAsyncFifoControl3, uint32_t> {
 public:
  // VDIN0_ASFIFO_CTRL3 has bits 31-24 used for control fields for VDI channel
  // 9 / async FIFO 9, which is for VIU writeback channel 2. Currently this
  // driver doesn't use this writeback channel, so we leave these bits reserved.

  // Bits 15-9 are control fields for VDI channel 7 / async FIFO 7.
  // Current this driver only doesn't use this VDI channel, so we leave these
  // bits reserved.

  // Enable data transmission on channel 6.
  DEF_BIT(7, channel6_data_enabled);

  // Enable go_field (Vsync) signals on channel 6.
  DEF_BIT(6, channel6_go_field_signal_enabled);

  // Enable go_line (Hsync) signals on channel 6.
  DEF_BIT(5, channel6_go_line_signal_enabled);

  // True iff the input video on channel 6 has negative polarity Vsync signals.
  DEF_BIT(4, channel6_input_vsync_is_negative);

  // True iff the input video on channel 6 has negative polarity Hsync signals.
  DEF_BIT(3, channel6_input_hsync_is_negative);

  // If set to true, the channel asynchronous FIFO will be reset on each Vsync
  // signal.
  DEF_BIT(2, channel6_async_fifo_software_reset_on_vsync);

  // Clears (and acknowledges) the `vdi6_fifo_overflow` bit in
  // `VDIN0/1_COM_STATUS2` register.
  DEF_BIT(1, channel6_clear_overflow_bit);

  // Resets the async FIFO.
  //
  // This bit is a "level signal", in order to reset the FIFO, drivers should
  // first write 1, and then write 0.
  DEF_BIT(0, channel6_async_fifo_software_reset);

  static auto Get(VideoInputModuleId video_input) {
    switch (video_input) {
      case VideoInputModuleId::kVideoInputModule0:
        static constexpr uint32_t kVideoInput0RegAddr = 0x126f * sizeof(uint32_t);
        return hwreg::RegisterAddr<VideoInputAsyncFifoControl3>(kVideoInput0RegAddr);
      case VideoInputModuleId::kVideoInputModule1:
        static constexpr uint32_t kVideoInput1RegAddr = 0x136f * sizeof(uint32_t);
        return hwreg::RegisterAddr<VideoInputAsyncFifoControl3>(kVideoInput1RegAddr);
      default:
        ZX_DEBUG_ASSERT_MSG(false, "Invalid video input module ID: %d",
                            static_cast<int>(video_input));
    }
  }
};

}  // namespace amlogic_display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_VIDEO_INPUT_REGS_H_
