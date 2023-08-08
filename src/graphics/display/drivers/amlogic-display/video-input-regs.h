// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_VIDEO_INPUT_REGS_H_
#define SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_VIDEO_INPUT_REGS_H_

#include <lib/ddk/debug.h>
#include <zircon/assert.h>

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

// There are multiple video input channels (VDIs, possibly shorthand for Video
// Data Input) available for video input modules (VDINs), numbered from VDI1 to
// VDI9. Each VDIN has one asynchronous FIFO (ASFIFO) for each VDI to receive
// pixels from.
//
// For each VDIN, there are four control registers (ASFIFO_CTRL0/1/2/3) to
// configure the way the VDIN reads pixels from a channel by setting the
// corresponding FIFO behaviors. The layout of the ASFIFO configuration fields
// is the same across all channels.
//
// Currently this driver only uses VDI 6 and VDI 8; control fields / registers
// for all the other VDI channels are not defined here.
//
// A311D Datasheet, Section 10.2.3.42 VDIN, Pages 1088-1090, 1106, 1110-1112,
// 1128.
// S905D2 Datasheet, Section 7.2.3.41 VDIN, Pages 779-781, 798-799, 802-805,
// 822.
// S905D3 Datasheet, Section 8.2.3.42 VDIN, Pages 715-717, 733, 738-740, 755.

// VDIN0_ASFIFO_CTRL2, VDIN1_ASFIFO_CTRL2
//
// A311D Datasheet, Section 10.2.3.42 VDIN, Pages 1090, 1112.
// S905D2 Datasheet, Section 7.2.3.41 VDIN, Pages 781, 805.
// S905D3 Datasheet, Section 8.2.3.42 VDIN, Pages 717, 740.
class VideoInputChannelFifoControl2
    : public hwreg::RegisterBase<VideoInputChannelFifoControl2, uint32_t> {
 public:
  // Bits 25 and 23-20 further configure decimation. This driver does not
  // support decimation, so we do not define the bits.

  // True iff input decimation subsampling is enabled.
  DEF_BIT(24, decimation_data_enabled);

  // Only 1 / (decimation_ratio_minus_1 + 1) of the pixels will be sampled.
  // Setting this field to zero effectively disables decimation.
  DEF_FIELD(19, 16, decimation_ratio_minus_1);

  // Bits 7-0 configure VDI 5.
  // Currently this driver doesn't use VDI channel 5, so we don't define these
  // bits.

  static auto Get(VideoInputModuleId video_input) {
    switch (video_input) {
      case VideoInputModuleId::kVideoInputModule0:
        static constexpr uint32_t kVideoInput0RegAddr = 0x120f * sizeof(uint32_t);
        return hwreg::RegisterAddr<VideoInputChannelFifoControl2>(kVideoInput0RegAddr);
      case VideoInputModuleId::kVideoInputModule1:
        static constexpr uint32_t kVideoInput1RegAddr = 0x130f * sizeof(uint32_t);
        return hwreg::RegisterAddr<VideoInputChannelFifoControl2>(kVideoInput1RegAddr);
    }
    ZX_DEBUG_ASSERT_MSG(false, "Invalid video input module ID: %d", static_cast<int>(video_input));
  }
};

// VDIN0_ASFIFO_CTRL3, VDIN1_ASFIFO_CTRL3
//
// A311D Datasheet, Section 10.2.3.42 VDIN, Pages 1106, 1128.
// S905D2 Datasheet, Section 7.2.3.41 VDIN, Pages 798-799, 822.
// S905D3 Datasheet, Section 8.2.3.42 VDIN, Pages 733, 755.
class VideoInputChannelFifoControl3
    : public hwreg::RegisterBase<VideoInputChannelFifoControl3, uint32_t> {
 public:
  // Bits 31-24 configure VDI 9.
  // Currently this driver doesn't use VDI channel 9, so we don't define these
  // bits.

  // Bits 23-16 configure VDI 8. These bits are not documented in any
  // datasheet. Experiments on a VIM3 (using Amlogic A311D) show that these
  // bits correspond to VDI 8 / ASFIFO 8.

  // Enable data transmission on channel 8.
  DEF_BIT(23, channel8_data_enabled);

  // Enable go_field (Vsync) signals on channel 8.
  DEF_BIT(22, channel8_go_field_signal_enabled);

  // Enable go_line (Hsync) signals for channel 8.
  DEF_BIT(21, channel8_go_line_signal_enabled);

  // True iff the input video on channel 8 has negative polarity Vsync signals.
  DEF_BIT(20, channel8_input_vsync_is_negative);

  // True iff the input video on channel 6 has negative polarity Hsync signals.
  DEF_BIT(19, channel8_input_hsync_is_negative);

  // If true, the ASFIFO will be reset on each Vsync signal.
  DEF_BIT(18, channel8_async_fifo_software_reset_on_vsync);

  // Clears (and acknowledges) the `vdi8_fifo_overflow` bit in the
  // `VDIN0/1_COM_STATUS2` register.
  DEF_BIT(17, channel8_clear_fifo_overflow_bit);

  // Resets the async FIFO.
  //
  // This bit is a "level signal" bit. Drivers reset the FIFO by first setting
  // it to 1, and then to 0.
  DEF_BIT(16, channel8_async_fifo_software_reset);

  // Bits 15-9 configure VDI 7.
  // Currently this driver doesn't use VDI channel 7, so we don't define these
  // bits.

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

  // If true, the channel ASFIFO will be reset on each Vsync signal.
  DEF_BIT(2, channel6_async_fifo_software_reset_on_vsync);

  // Clears (and acknowledges) the `vdi6_fifo_overflow` bit in the
  // `VDIN0/1_COM_STATUS2` register.
  DEF_BIT(1, channel6_clear_fifo_overflow_bit);

  // Resets the async FIFO.
  //
  // This bit is a "level signal" bit. Drivers reset the FIFO by first setting
  // it to 1, and then to 0.
  DEF_BIT(0, channel6_async_fifo_software_reset);

  static auto Get(VideoInputModuleId video_input) {
    switch (video_input) {
      case VideoInputModuleId::kVideoInputModule0:
        static constexpr uint32_t kVideoInput0RegAddr = 0x126f * sizeof(uint32_t);
        return hwreg::RegisterAddr<VideoInputChannelFifoControl3>(kVideoInput0RegAddr);
      case VideoInputModuleId::kVideoInputModule1:
        static constexpr uint32_t kVideoInput1RegAddr = 0x136f * sizeof(uint32_t);
        return hwreg::RegisterAddr<VideoInputChannelFifoControl3>(kVideoInput1RegAddr);
      default:
        ZX_DEBUG_ASSERT_MSG(false, "Invalid video input module ID: %d",
                            static_cast<int>(video_input));
    }
  }
};

// Selects the clock or data source for a writeback mux.
//
// Fields of this type must transition through `kDisabled` when being updated.
enum class WritebackMuxSource : uint32_t {
  // Disable the input path. A required intermediate step for changing input
  // sources.
  kDisabled = 0b00000,
  // VIU ENCI domain.
  kEncoderInterlaced = 0b00001,
  // VIU ENCP domain.
  kEncoderProgressive = 0b00010,
  // VIU ENCT domain.
  kEncoderTvPanel = 0b00100,
  // Also known as "VIU writeback domain 1" in S905D3 datasheets.
  kViuWriteback0 = 0b01000,
  // Also known as "VIU writeback domain 2" in S905D3 datasheets.
  kViuWriteback1 = 0b10000,
};

// VPU_VIU_VDIN_IF_MUX_CTRL
//
// Each VDIN (Video Input Module) can receive image data from one of the data
// sources available on the SoC, selected via the `VideoInputCommandControl`
// register. Two of the data sources, `kWritebackMux0` and `kWritebackMux1`,
// are actually multiplexers connected to multiple sources.
//
// This register configures the multiplexers. Each multiplexer has clock and
// data outputs ("paths" in the datasheets), which are configured separately.
//
// Many configurations are invalid. For example, a multiplexer's clock and data
// outputs must be connected to the same source. To reduce the likelihood of
// errors, this register's fields should be accessed exclusively via the
// higher-level helper methods defined after the fields.
//
// S905D3 Datasheet, Section 8.2.3.1 "VPU Registers", Page 314.
//
// This register is not documented in the datasheets for S905D2, A311D and T931.
// However, experiments on VIM3 (Amlogic A311D), Astro (Amlogic S905D2) and
// Sherlock (Amlogic T931) show that the register exists and has the same layout
// and functionality as that in S905D3.
class WritebackMuxControl : public hwreg::RegisterBase<WritebackMuxControl, uint32_t> {
 public:  // Selects the data path from VIU/Encoder to writeback mux 1.
  // This field is effective when a `VideoInputCommandControl` register selects
  // the `kWritebackMux1` input ("VDIN0/1 source input 9" in the datasheet).
  //
  // It's preferred to use `GetMux1Selection()` and `SetMux1Selection()` helper
  // functions to change the data path. For direct register manipulations,
  // `mux1_data_selection` and `mux1_clock_selection` must be equal, otherwise
  // the behavior is undefined.
  DEF_ENUM_FIELD(WritebackMuxSource, 28, 24, mux1_data_selection);

  // Selects the clock path from VIU/Encoder to writeback mux 1.
  // This field is effective when a `VideoInputCommandControl` register selects
  // the `kWritebackMux1` input ("VDIN0/1 source input 9" in the datasheet).
  //
  // It's preferred to use `GetMux1Selection()` and `SetMux1Selection()` helper
  // functions to change the data path. For direct register manipulations,
  // `mux1_data_selection` and `mux1_clock_selection` must be equal, otherwise
  // the behavior is undefined.
  DEF_ENUM_FIELD(WritebackMuxSource, 20, 16, mux1_clock_selection);

  // Selects the data path from VIU/Encoder to writeback mux 0.
  // This field is effective when a `VideoInputCommandControl` register selects
  // the `kWritebackMux0` input ("VDIN0/1 source input 7" in the datasheet).
  //
  // It's preferred to use `GetMux0Selection()` and `SetMux0Selection()` helper
  // functions to change the data path. For direct register manipulations,
  // `mux0_data_selection` and `mux0_clock_selection` must be equal, otherwise
  // the behavior is undefined.
  DEF_ENUM_FIELD(WritebackMuxSource, 12, 8, mux0_data_selection);

  // Selects the clock path from VIU/Encoder to writeback mux 0.
  // This field is effective when a `VideoInputCommandControl` register selects
  // the `kWritebackMux0` input ("VDIN0/1 source input 7" in the datasheet).
  //
  // It's preferred to use `GetMux0Selection()` and `SetMux0Selection()` helper
  // functions to change the data path. For direct register manipulations,
  // `mux0_data_selection` and `mux0_clock_selection` must be equal, otherwise
  // the behavior is WritebackMuxSelection.
  DEF_ENUM_FIELD(WritebackMuxSource, 4, 0, mux0_clock_selection);

  // The clock/data source selected for writeback mux 1.
  WritebackMuxSource GetMux1Selection() const {
    WritebackMuxSource clock = mux1_clock_selection();
    WritebackMuxSource data = mux1_data_selection();
    if (clock != data) {
      zxlogf(WARNING, "Writeback mux1 clock selection %" PRIu32 " != data selection %" PRIu32,
             static_cast<uint32_t>(clock), static_cast<uint32_t>(data));
    }
    return clock;
  }

  // Set the data/clock source for writeback mux 1.
  WritebackMuxControl& SetMux1Selection(WritebackMuxSource mux_selection) {
    ZX_ASSERT(GetMux1Selection() == WritebackMuxSource::kDisabled ||
              mux_selection == WritebackMuxSource::kDisabled);
    switch (mux_selection) {
      case WritebackMuxSource::kDisabled:
      case WritebackMuxSource::kEncoderInterlaced:
      case WritebackMuxSource::kEncoderProgressive:
      case WritebackMuxSource::kEncoderTvPanel:
      case WritebackMuxSource::kViuWriteback0:
      case WritebackMuxSource::kViuWriteback1:
        set_mux1_clock_selection(mux_selection);
        set_mux1_data_selection(mux_selection);
        return *this;
    }
    ZX_DEBUG_ASSERT_MSG(false, "Invalid mux selection %" PRIu32,
                        static_cast<uint32_t>(mux_selection));
    return *this;
  }

  // The clock/data source selected for writeback mux 0.
  WritebackMuxSource GetMux0Selection() const {
    WritebackMuxSource clock = mux0_clock_selection();
    WritebackMuxSource data = mux0_data_selection();
    if (clock != data) {
      zxlogf(WARNING, "Writeback mux0 clock selection %" PRIu32 " != data selection %" PRIu32,
             static_cast<uint32_t>(clock), static_cast<uint32_t>(data));
    }
    return clock;
  }

  // Set the data/clock source for writeback mux 0.
  WritebackMuxControl& SetMux0Selection(WritebackMuxSource mux_selection) {
    ZX_ASSERT(GetMux0Selection() == WritebackMuxSource::kDisabled ||
              mux_selection == WritebackMuxSource::kDisabled);
    switch (mux_selection) {
      case WritebackMuxSource::kDisabled:
      case WritebackMuxSource::kEncoderInterlaced:
      case WritebackMuxSource::kEncoderProgressive:
      case WritebackMuxSource::kEncoderTvPanel:
      case WritebackMuxSource::kViuWriteback0:
      case WritebackMuxSource::kViuWriteback1:
        set_mux0_clock_selection(mux_selection);
        set_mux0_data_selection(mux_selection);
        return *this;
    }
    ZX_DEBUG_ASSERT_MSG(false, "Invalid mux selection %" PRIu32,
                        static_cast<uint32_t>(mux_selection));
    return *this;
  }

  static auto Get() { return hwreg::RegisterAddr<WritebackMuxControl>(0x2783 * sizeof(uint32_t)); }
};

}  // namespace amlogic_display

#endif  // SRC_GRAPHICS_DISPLAY_DRIVERS_AMLOGIC_DISPLAY_VIDEO_INPUT_REGS_H_
