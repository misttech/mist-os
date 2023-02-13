// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BLOCK_DRIVERS_IMX8M_SDMMC_IMX8M_SDMMC_REGS_H_
#define SRC_DEVICES_BLOCK_DRIVERS_IMX8M_SDMMC_IMX8M_SDMMC_REGS_H_

#include <hwreg/bitfields.h>

namespace imx8m_sdmmc {

constexpr size_t kRegisterSetSize = 260;

class DMASystemAddress : public hwreg::RegisterBase<DMASystemAddress, uint32_t> {
 public:
  static auto Get() { return hwreg::RegisterAddr<DMASystemAddress>(0x00); }
};

class BlockAttributes : public hwreg::RegisterBase<BlockAttributes, uint32_t> {
 public:
  static constexpr uint32_t kMaxBlockSize = 4096;
  static constexpr uint32_t kMaxBlockCount = 65535;

  static auto Get() { return hwreg::RegisterAddr<BlockAttributes>(0x04); }

  DEF_FIELD(31, 16, block_count);
  DEF_RSVDZ_FIELD(15, 13);
  DEF_FIELD(12, 0, block_size);
};

class CommandArgument : public hwreg::RegisterBase<CommandArgument, uint32_t> {
 public:
  static auto Get() { return hwreg::RegisterAddr<CommandArgument>(0x08); }

  DEF_FIELD(31, 0, cmd_arg);
};

class CommandTransferType : public hwreg::RegisterBase<CommandTransferType, uint32_t> {
 public:
  static constexpr uint32_t kResponseTypeNone = 0b00;
  static constexpr uint32_t kResponseType136Bits = 0b01;
  static constexpr uint32_t kResponseType48Bits = 0b10;
  static constexpr uint32_t kResponseType48BitsWithBusy = 0b11;

  static constexpr uint32_t kCommandTypeNormal = 0b00;
  static constexpr uint32_t kCommandTypeSuspend = 0b01;
  static constexpr uint32_t kCommandTypeResume = 0b10;
  static constexpr uint32_t kCommandTypeAbort = 0b11;

  static auto Get() { return hwreg::RegisterAddr<CommandTransferType>(0x0c); }

  DEF_RSVDZ_FIELD(31, 30);
  DEF_FIELD(29, 24, cmd_index);
  DEF_FIELD(23, 22, cmd_type);
  DEF_BIT(21, data_present);
  DEF_BIT(20, cmd_index_check);
  DEF_BIT(19, cmd_crc_check);
  DEF_RSVDZ_BIT(18);
  DEF_FIELD(17, 16, response_type);
  DEF_RSVDZ_FIELD(15, 0);
};

class Response : public hwreg::RegisterBase<Response, uint32_t> {
 public:
  static auto Get(uint32_t index) { return hwreg::RegisterAddr<Response>(0x10 + (index * 4)); }
};

class BufferData : public hwreg::RegisterBase<BufferData, uint32_t> {
 public:
  static auto Get() { return hwreg::RegisterAddr<BufferData>(0x20); }
};

class PresentState : public hwreg::RegisterBase<PresentState, uint32_t> {
 public:
  static auto Get() { return hwreg::RegisterAddr<PresentState>(0x24); }

  DEF_FIELD(31, 24, data_line_signal_level);
  DEF_BIT(23, cmd_line_signal_level);
  DEF_RSVDZ_FIELD(22, 20);
  DEF_BIT(19, wp_switch_pin_level);
  DEF_BIT(18, card_detect_pin_level);
  DEF_RSVDZ_BIT(17);
  DEF_BIT(16, card_inserted);
  DEF_BIT(15, tape_select_change_done);
  DEF_RSVDZ_FIELD(14, 13);
  DEF_BIT(12, retuning_request);
  DEF_BIT(11, buffer_read_enable);
  DEF_BIT(10, buffer_write_enable);
  DEF_BIT(9, read_transfer_active);
  DEF_BIT(8, write_transfer_active);
  DEF_BIT(7, sd_clock_gated_off);
  DEF_BIT(6, ipgper_clock_gated_off);
  DEF_BIT(5, hclk_gated_off);
  DEF_BIT(4, peripheral_clock_gated_off);
  DEF_BIT(3, sd_clock_stable);
  DEF_BIT(2, data_line_active);
  DEF_BIT(1, cmd_inhibit_data);
  DEF_BIT(0, cmd_inhibit_cmd);
};

class ProtocolControl : public hwreg::RegisterBase<ProtocolControl, uint32_t> {
 public:
  static constexpr uint32_t kBusWidth1Bit = 0;
  static constexpr uint32_t kBusWidth4Bit = 1;
  static constexpr uint32_t kBusWidth8Bit = 2;

  static constexpr uint32_t kDmaSelNoDma = 0;
  static constexpr uint32_t kDmaSelAdma1 = 1;
  static constexpr uint32_t kDmaSelAdma2 = 2;

  static auto Get() { return hwreg::RegisterAddr<ProtocolControl>(0x28); }

  DEF_RSVDZ_BIT(31);
  DEF_BIT(30, non_exact_block_read);
  DEF_FIELD(29, 27, burst_length_enable);
  DEF_BIT(26, wakeup_on_sd_removal);
  DEF_BIT(25, wakeup_on_sd_insertion);
  DEF_BIT(24, wakeup_on_card_interrupt);
  DEF_RSVDZ_FIELD(23, 21);
  DEF_BIT(20, read_data_8clock_disable);
  DEF_BIT(19, interrupt_at_block_gap);
  DEF_BIT(18, read_wait_control);
  DEF_BIT(17, continue_request);
  DEF_BIT(16, stop_at_block_gap_request);
  DEF_RSVDZ_FIELD(15, 10);
  DEF_FIELD(9, 8, dma_select);
  DEF_BIT(7, card_detect_signal_select);
  DEF_BIT(6, card_detect_test_level);
  DEF_FIELD(5, 4, endian_mode);
  DEF_BIT(3, data3_as_card_detect);
  DEF_FIELD(2, 1, data_transfer_width);
  DEF_BIT(0, led_control);
};

class SystemControl : public hwreg::RegisterBase<SystemControl, uint32_t> {
 public:
  static auto Get() { return hwreg::RegisterAddr<SystemControl>(0x2c); }

  DEF_RSVDZ_FIELD(31, 29);
  DEF_BIT(28, reset_tuning);
  DEF_BIT(27, initialization_active);
  DEF_BIT(26, reset_data);
  DEF_BIT(25, reset_cmd);
  DEF_BIT(24, reset_all);
  DEF_BIT(23, hardware_reset);
  DEF_RSVDZ_BIT(22);
  DEF_RSVDZ_BIT(21);
  DEF_RSVDZ_BIT(20);
  DEF_FIELD(19, 16, data_timeout_counter_value);
  DEF_FIELD(15, 8, sdclk_prescaler);
  DEF_FIELD(7, 4, sdclk_divisor);
  DEF_FIELD(3, 0, clocks);
};

class InterruptStatus : public hwreg::RegisterBase<InterruptStatus, uint32_t> {
 public:
  static auto Get() { return hwreg::RegisterAddr<InterruptStatus>(0x30); }

  auto& ClearAll() { return set_reg_value(0xffffffff); }

  bool ErrorInterrupt() const {
    return tuning_error() || dma_error() || auto_cmd_error() || data_end_bit_error() ||
           data_crc_error() || data_timeout_error() || cmd_index_error() || cmd_end_bit_error() ||
           cmd_crc_error() || cmd_timeout_error();
  }

  DEF_RSVDZ_FIELD(31, 29);
  DEF_BIT(28, dma_error);
  DEF_RSVDZ_BIT(27);
  DEF_BIT(26, tuning_error);
  DEF_RSVDZ_BIT(25);
  DEF_BIT(24, auto_cmd_error);
  DEF_RSVDZ_BIT(23);
  DEF_BIT(22, data_end_bit_error);
  DEF_BIT(21, data_crc_error);
  DEF_BIT(20, data_timeout_error);
  DEF_BIT(19, cmd_index_error);
  DEF_BIT(18, cmd_end_bit_error);
  DEF_BIT(17, cmd_crc_error);
  DEF_BIT(16, cmd_timeout_error);
  DEF_RSVDZ_BIT(15);
  DEF_BIT(14, cmd_queuing_interrupt);
  DEF_BIT(13, tuning_pass);
  DEF_BIT(12, retuning_event);
  DEF_RSVDZ_FIELD(11, 9);
  DEF_BIT(8, card_interrupt);
  DEF_BIT(7, card_removal);
  DEF_BIT(6, card_insertion);
  DEF_BIT(5, buffer_read_ready);
  DEF_BIT(4, buffer_write_ready);
  DEF_BIT(3, dma_interrupt);
  DEF_BIT(2, block_gap_event);
  DEF_BIT(1, transfer_complete);
  DEF_BIT(0, cmd_complete);
};

class InterruptStatusEnable : public hwreg::RegisterBase<InterruptStatusEnable, uint32_t> {
 public:
  static auto Get() { return hwreg::RegisterAddr<InterruptStatusEnable>(0x34); }

  auto& EnableErrorInterrupts() {
    return set_tuning_error(1)
        .set_dma_error(1)
        .set_auto_cmd_error(1)
        .set_data_end_bit_error(1)
        .set_data_crc_error(1)
        .set_data_timeout_error(1)
        .set_cmd_index_error(1)
        .set_cmd_end_bit_error(1)
        .set_cmd_crc_error(1)
        .set_cmd_timeout_error(1);
  }

  auto& EnableNormalInterrupts() {
    return set_card_interrupt(0)
        .set_card_removal(0)
        .set_card_insertion(0)
        .set_buffer_read_ready(1)
        .set_buffer_write_ready(1)
        .set_dma_interrupt(1)
        .set_block_gap_event(1)
        .set_transfer_complete(1)
        .set_cmd_complete(1);
  }

  DEF_RSVDZ_FIELD(31, 29);
  DEF_BIT(28, dma_error);
  DEF_RSVDZ_BIT(27);
  DEF_BIT(26, tuning_error);
  DEF_RSVDZ_BIT(25);
  DEF_BIT(24, auto_cmd_error);
  DEF_RSVDZ_BIT(23);
  DEF_BIT(22, data_end_bit_error);
  DEF_BIT(21, data_crc_error);
  DEF_BIT(20, data_timeout_error);
  DEF_BIT(19, cmd_index_error);
  DEF_BIT(18, cmd_end_bit_error);
  DEF_BIT(17, cmd_crc_error);
  DEF_BIT(16, cmd_timeout_error);
  DEF_RSVDZ_BIT(15);
  DEF_BIT(14, cmd_queuing_interrupt);
  DEF_BIT(13, tuning_pass);
  DEF_BIT(12, retuning_event);
  DEF_RSVDZ_FIELD(11, 9);
  DEF_BIT(8, card_interrupt);
  DEF_BIT(7, card_removal);
  DEF_BIT(6, card_insertion);
  DEF_BIT(5, buffer_read_ready);
  DEF_BIT(4, buffer_write_ready);
  DEF_BIT(3, dma_interrupt);
  DEF_BIT(2, block_gap_event);
  DEF_BIT(1, transfer_complete);
  DEF_BIT(0, cmd_complete);
};

class InterruptSignalEnable : public hwreg::RegisterBase<InterruptSignalEnable, uint32_t> {
 public:
  static auto Get() { return hwreg::RegisterAddr<InterruptSignalEnable>(0x38); }

  auto& MaskAll() { return set_reg_value(0x0000'0000); }

  auto& EnableErrorInterrupts() {
    return set_tuning_error(1)
        .set_dma_error(1)
        .set_auto_cmd_error(1)
        .set_data_end_bit_error(1)
        .set_data_crc_error(1)
        .set_data_timeout_error(1)
        .set_cmd_index_error(1)
        .set_cmd_end_bit_error(1)
        .set_cmd_crc_error(1)
        .set_cmd_timeout_error(1);
  }

  auto& EnableNormalInterrupts() {
    return set_card_interrupt(1)
        .set_card_removal(0)
        .set_card_insertion(0)
        .set_buffer_read_ready(1)
        .set_buffer_write_ready(1)
        .set_dma_interrupt(1)
        .set_block_gap_event(1)
        .set_transfer_complete(1)
        .set_cmd_complete(1);
  }

  DEF_RSVDZ_FIELD(31, 29);
  DEF_BIT(28, dma_error);
  DEF_RSVDZ_BIT(27);
  DEF_BIT(26, tuning_error);
  DEF_RSVDZ_BIT(25);
  DEF_BIT(24, auto_cmd_error);
  DEF_RSVDZ_BIT(23);
  DEF_BIT(22, data_end_bit_error);
  DEF_BIT(21, data_crc_error);
  DEF_BIT(20, data_timeout_error);
  DEF_BIT(19, cmd_index_error);
  DEF_BIT(18, cmd_end_bit_error);
  DEF_BIT(17, cmd_crc_error);
  DEF_BIT(16, cmd_timeout_error);
  DEF_RSVDZ_BIT(15);
  DEF_BIT(14, cmd_queuing_interrupt);
  DEF_BIT(13, tuning_pass);
  DEF_BIT(12, retuning_event);
  DEF_RSVDZ_FIELD(11, 9);
  DEF_BIT(8, card_interrupt);
  DEF_BIT(7, card_removal);
  DEF_BIT(6, card_insertion);
  DEF_BIT(5, buffer_read_ready);
  DEF_BIT(4, buffer_write_ready);
  DEF_BIT(3, dma_interrupt);
  DEF_BIT(2, block_gap_event);
  DEF_BIT(1, transfer_complete);
  DEF_BIT(0, cmd_complete);
};

class AutoCmd12ErrorStatus : public hwreg::RegisterBase<AutoCmd12ErrorStatus, uint32_t> {
 public:
  static auto Get() { return hwreg::RegisterAddr<AutoCmd12ErrorStatus>(0x3c); }

  DEF_RSVDZ_FIELD(31, 24);
  DEF_BIT(23, sample_clock_select);
  DEF_BIT(22, execute_tuning);
  DEF_RSVDZ_FIELD(21, 8);
  DEF_BIT(7, cmd_not_issued);
  DEF_RSVDZ_FIELD(6, 5);
  DEF_BIT(4, auto_cmd_12_23_index_error);
  DEF_BIT(3, auto_cmd_12_23_crc_error);
  DEF_BIT(2, auto_cmd_12_23_end_bit_error);
  DEF_BIT(1, auto_cmd_12_23_timeout_error);
  DEF_BIT(0, auto_cmd_12_not_executed);
};

class HostControllerCapabilities
    : public hwreg::RegisterBase<HostControllerCapabilities, uint32_t> {
 public:
  static auto Get() { return hwreg::RegisterAddr<HostControllerCapabilities>(0x40); }

  DEF_RSVDZ_FIELD(31, 27);
  DEF_BIT(26, voltage_1v8_support);
  DEF_BIT(25, voltage_3v0_support);
  DEF_BIT(24, voltage_3v3_support);
  DEF_BIT(23, suspend_resume_support);
  DEF_BIT(22, dma_support);
  DEF_BIT(21, high_speed_support);
  DEF_BIT(20, adma_support);
  DEF_RSVDZ_BIT(19);
  DEF_FIELD(18, 16, max_block_length);
  DEF_FIELD(15, 14, retuning_mode);
  DEF_BIT(13, use_tuning_for_sdr50);
  DEF_RSVDZ_BIT(12);
  DEF_FIELD(11, 8, time_counter_for_retuning);
  DEF_RSVDZ_FIELD(7, 3);
  DEF_BIT(2, ddr50_support);
  DEF_BIT(1, sdr104_support);
  DEF_BIT(0, sdr50_support);
};

class WatermarkLevel : public hwreg::RegisterBase<WatermarkLevel, uint32_t> {
 public:
  static auto Get() { return hwreg::RegisterAddr<WatermarkLevel>(0x44); }

  DEF_RSVDZ_FIELD(31, 29);
  DEF_FIELD(28, 24, write_burst_length);
  DEF_FIELD(23, 16, write_watermark_level);
  DEF_RSVDZ_FIELD(15, 13);
  DEF_FIELD(12, 8, read_burst_length);
  DEF_FIELD(7, 0, read_watermark_level);
};

class MixerControl : public hwreg::RegisterBase<MixerControl, uint32_t> {
 public:
  static auto Get() { return hwreg::RegisterAddr<MixerControl>(0x48); }

  DEF_BIT(31, enable);
  DEF_RSVDZ_BIT(30);
  DEF_RSVDZ_BIT(29);
  DEF_RSVDZ_BIT(28);
  DEF_BIT(27, enhance_hs400_enable);
  DEF_BIT(26, hs400_enable);
  DEF_BIT(25, feedback_clk_src_select);
  DEF_BIT(24, auto_tuning_enable);
  DEF_BIT(23, clock_select);
  DEF_BIT(22, execute_tuning);
  DEF_RSVDZ_FIELD(21, 8);
  DEF_BIT(7, auto_cmd23_enable);
  DEF_BIT(6, nibble_position_indication);
  DEF_BIT(5, multi_single_block_select);
  DEF_BIT(4, data_transfer_dir_select);
  DEF_BIT(3, ddr_mode_select);
  DEF_BIT(2, auto_cmd12_enable);
  DEF_BIT(1, block_count_enable);
  DEF_BIT(0, dma_enable);
};

class ForceEvent : public hwreg::RegisterBase<ForceEvent, uint32_t> {
 public:
  static auto Get() { return hwreg::RegisterAddr<ForceEvent>(0x50); }

  DEF_BIT(31, card_interrupt);
  DEF_RSVDZ_FIELD(30, 29);
  DEF_BIT(28, dma_error);
  DEF_RSVDZ_BIT(27);
  DEF_BIT(26, tuning_error);
  DEF_RSVDZ_BIT(25);
  DEF_BIT(24, auto_cmd_12_error);
  DEF_RSVDZ_BIT(23);
  DEF_BIT(22, data_end_bit_error);
  DEF_BIT(21, data_crc_error);
  DEF_BIT(20, data_timeout_error);
  DEF_BIT(19, cmd_index_error);
  DEF_BIT(18, cmd_end_bit_error);
  DEF_BIT(17, cmd_crc_error);
  DEF_BIT(16, cmd_timeout_error);
  DEF_RSVDZ_FIELD(15, 8);
  DEF_BIT(7, cmd_not_issued_due_to_autocmd12_error);
  DEF_RSVDZ_FIELD(6, 5);
  DEF_BIT(4, auto_cmd_12_23_index_error);
  DEF_BIT(3, auto_cmd_12_23_crc_error);
  DEF_BIT(2, auto_cmd_12_23_end_bit_error);
  DEF_BIT(1, auto_cmd_12_23_timeout_error);
  DEF_BIT(0, auto_cmd_12_not_executed);
};

class AdmaErrorStatus : public hwreg::RegisterBase<AdmaErrorStatus, uint32_t> {
 public:
  static auto Get() { return hwreg::RegisterAddr<AdmaErrorStatus>(0x54); }

  DEF_RSVDZ_FIELD(31, 4);
  DEF_BIT(3, adma_desc_error);
  DEF_BIT(2, adma_length_mismatch_error);
  DEF_FIELD(1, 0, adma_error_state);
};

class AdmaSystemAddress : public hwreg::RegisterBase<AdmaSystemAddress, uint32_t> {
 public:
  static auto Get() { return hwreg::RegisterAddr<AdmaSystemAddress>(0x58); }
};

class DelayLineControl : public hwreg::RegisterBase<DelayLineControl, uint32_t> {
 public:
  static auto Get() { return hwreg::RegisterAddr<DelayLineControl>(0x60); }

  DEF_FIELD(31, 28, loop_update_interval);
  DEF_FIELD(27, 20, slave_update_interval);
  DEF_RSVDZ_BIT(19);
  DEF_FIELD(18, 16, slave_delay_target1);
  DEF_FIELD(15, 9, slave_override_val);
  DEF_BIT(8, slave_override);
  DEF_BIT(7, gate_update);
  DEF_FIELD(6, 3, slave_delay_target0);
  DEF_BIT(2, slave_delay_line);
  DEF_BIT(1, reset);
  DEF_BIT(0, enable);
};

class DelayLineStatus : public hwreg::RegisterBase<DelayLineStatus, uint32_t> {
 public:
  static auto Get() { return hwreg::RegisterAddr<DelayLineStatus>(0x64); }

  DEF_RSVDZ_FIELD(31, 16);
  DEF_FIELD(15, 9, reference_dll_select_taps);
  DEF_FIELD(8, 2, slave_dll_select_status);
  DEF_BIT(1, reference_dll_lock_status);
  DEF_BIT(0, slave_dll_lock_status);
};

class ClockTuningControlStatus : public hwreg::RegisterBase<ClockTuningControlStatus, uint32_t> {
 public:
  static auto Get() { return hwreg::RegisterAddr<ClockTuningControlStatus>(0x68); }

  DEF_BIT(31, pre_error);
  DEF_FIELD(30, 24, tap_sel_pre);
  DEF_FIELD(23, 20, tap_sel_out);
  DEF_FIELD(19, 16, tap_sel_post);
  DEF_BIT(15, nxt_error);
  DEF_FIELD(14, 8, dly_cell_set_pre);
  DEF_FIELD(7, 4, dly_cell_set_out);
  DEF_FIELD(3, 0, dly_cell_set_post);
};

class StrobeDelayLineControl : public hwreg::RegisterBase<StrobeDelayLineControl, uint32_t> {
 public:
  static auto Get() { return hwreg::RegisterAddr<StrobeDelayLineControl>(0x70); }

  DEF_FIELD(31, 28, loop_update_interval);
  DEF_FIELD(27, 20, slave_update_interval);
  DEF_RSVDZ_FIELD(19, 16);
  DEF_FIELD(15, 9, slave_override_val);
  DEF_BIT(8, slave_override);
  DEF_BIT(7, gate_update0);
  DEF_BIT(6, gate_update1);
  DEF_FIELD(5, 3, slave_delay_target);
  DEF_BIT(2, slave_force_update);
  DEF_BIT(1, reset);
  DEF_BIT(0, enable);
};

class StrobeDelayLineStatus : public hwreg::RegisterBase<StrobeDelayLineStatus, uint32_t> {
 public:
  static auto Get() { return hwreg::RegisterAddr<StrobeDelayLineStatus>(0x74); }

  DEF_RSVDZ_FIELD(31, 16);
  DEF_FIELD(15, 9, reference_dll_select_taps);
  DEF_FIELD(8, 2, slave_dll_select_status);
  DEF_BIT(1, reference_dll_lock_status);
  DEF_BIT(0, slave_dll_lock_status);
};

class VendorSpecificRegister : public hwreg::RegisterBase<VendorSpecificRegister, uint32_t> {
 public:
  static auto Get() { return hwreg::RegisterAddr<VendorSpecificRegister>(0xc0); }

  DEF_BIT(31, cmd_byte_enable);
  DEF_RSVDZ_BIT(30);
  DEF_BIT(29, enable);
  DEF_RSVDZ_BIT(28);
  DEF_RSVDZ_FIELD(27, 24);
  DEF_RSVDZ_FIELD(23, 16);
  DEF_BIT(15, crc_check_disable);
  DEF_FIELD(14, 11, clocks);
  DEF_RSVDZ_BIT(10);
  DEF_RSVDZ_BIT(9);
  DEF_BIT(8, force_clk);
  DEF_RSVDZ_FIELD(7, 4);
  DEF_BIT(3, check_busy_enable);
  DEF_BIT(2, conflict_check_enable);
  DEF_BIT(1, voltage_select);
  DEF_BIT(0, external_dma_req_enable);
};

class MmcBoot : public hwreg::RegisterBase<MmcBoot, uint32_t> {
 public:
  static auto Get() { return hwreg::RegisterAddr<MmcBoot>(0xc4); }

  DEF_FIELD(31, 16, sabg_block_count);
  DEF_RSVDZ_FIELD(15, 9);
  DEF_BIT(8, timeout_disable);
  DEF_BIT(7, auto_sabg);
  DEF_BIT(6, boot_enable);
  DEF_BIT(5, boot_mode);
  DEF_BIT(4, boot_ack);
  DEF_FIELD(3, 0, boot_ack_timeout_cnt_val);
};

class VendorSpecificRegister2 : public hwreg::RegisterBase<VendorSpecificRegister2, uint32_t> {
 public:
  static constexpr uint16_t kTuning1Bit = 0b10;
  static constexpr uint16_t kTuning4Bit = 0b00;
  static constexpr uint16_t kTuning8Bit = 0b01;

  static auto Get() { return hwreg::RegisterAddr<VendorSpecificRegister2>(0xc8); }

  DEF_FIELD(31, 16, fbclk_tap_sel);
  DEF_BIT(15, enable_32khz_clock);
  DEF_RSVDZ_BIT(14);
  DEF_RSVDZ_BIT(13);
  DEF_BIT(12, acmd23_argu2_enable);
  DEF_BIT(11, hs400_read_clock_stop_enable);
  DEF_BIT(10, hs400_write_clock_stop_enable);
  DEF_RSVDZ_BIT(9);
  DEF_BIT(8, busy_rsp_irq);
  DEF_RSVDZ_BIT(7);
  DEF_BIT(6, tuning_cmd_enable);
  DEF_FIELD(5, 4, tuning_data_enable);
  DEF_BIT(3, card_interrupt_d3_test);
  DEF_RSVDZ_FIELD(2, 0);
};

class TuningControl : public hwreg::RegisterBase<TuningControl, uint32_t> {
 public:
  static auto Get() { return hwreg::RegisterAddr<TuningControl>(0xcc); }

  DEF_RSVDZ_FIELD(31, 25);
  DEF_BIT(24, std_tuning_enable);
  DEF_RSVDZ_BIT(23);
  DEF_FIELD(22, 20, data_window);
  DEF_RSVDZ_BIT(19);
  DEF_FIELD(18, 16, tuning_step);
  DEF_FIELD(15, 8, tuning_counter);
  DEF_BIT(7, tuning_cmd_crc_check_disable);
  DEF_FIELD(6, 0, tuning_start_tap);
};

class CommandQueue : public hwreg::RegisterBase<CommandQueue, uint32_t> {
 public:
  static auto Get() { return hwreg::RegisterAddr<CommandQueue>(0x100); }

  DEF_RSVDZ_FIELD(31, 0);
};

}  // namespace imx8m_sdmmc

#endif  // SRC_DEVICES_BLOCK_DRIVERS_IMX8M_SDMMC_IMX8M_SDMMC_REGS_H_
