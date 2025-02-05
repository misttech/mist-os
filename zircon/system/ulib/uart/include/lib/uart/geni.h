// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_UART_GENI_H_
#define LIB_UART_GENI_H_

#include <lib/stdcompat/array.h>
#include <lib/zbi-format/driver-config.h>
#include <lib/zbi-format/zbi.h>

#include <algorithm>

#include <hwreg/bitfields.h>

#include "interrupt.h"
#include "uart.h"

namespace uart::geni {

struct FifoRegister : public hwreg::RegisterBase<FifoRegister, uint32_t> {
  DEF_FIELD(31, 0, data);
  static auto Get(uint32_t offset) { return hwreg::RegisterAddr<FifoRegister>(offset); }
};

struct TxFifoRegister {
  static auto Get() { return FifoRegister::Get(0x700); }
};

struct RxFifoRegister {
  static auto Get() { return FifoRegister::Get(0x780); }
};

struct ByteFifoRegister : public hwreg::RegisterBase<ByteFifoRegister, uint32_t> {
  DEF_FIELD(7, 0, byte);
  DEF_RSVDZ_FIELD(31, 8);
  static auto Get(uint32_t offset) { return hwreg::RegisterAddr<ByteFifoRegister>(offset); }
};

struct TxByteFifoRegister {
  static auto Get() { return ByteFifoRegister::Get(0x700); }
};

struct RxByteFifoRegister {
  static auto Get() { return ByteFifoRegister::Get(0x780); }
};

struct GeniStatusRegister : public hwreg::RegisterBase<GeniStatusRegister, uint32_t> {
  DEF_BIT(0, m_command_active);
  DEF_FIELD(8, 4, m_command_interface_state);
  DEF_BIT(12, s_command_active);
  DEF_FIELD(20, 16, s_command_interface_state);
  static auto Get() { return hwreg::RegisterAddr<GeniStatusRegister>(0x40); }
};

struct ClockRegister : public hwreg::RegisterBase<ClockRegister, uint32_t> {
  DEF_BIT(0, enable);
  DEF_FIELD(31, 4, div);
  static auto Get(uint32_t offset) { return hwreg::RegisterAddr<ClockRegister>(offset); }
};

struct MainClockRegister {
  static auto Get() { return ClockRegister::Get(0x48); }
};

struct SecondaryClockRegister {
  static auto Get() { return ClockRegister::Get(0x4c); }
};

// RX_FIFO_STATUS / GENI_RX_FIFO_STATUS
//
// This register is not documented in the Hardware register description
// doc, but is referred in both the Hardware register description and the QUP
// v3 Hardware Programming Guide.
//
// Experiments show that the register definitions below match the hardware
// behavior.
struct RxFifoStatusRegister : public hwreg::RegisterBase<RxFifoStatusRegister, uint32_t> {
  static auto Get() { return hwreg::RegisterAddr<RxFifoStatusRegister>(0x804); }

  // Determines if the FIFO has received the last byte.
  DEF_BIT(31, last_byte_received);

  // If not zero, number of valid bytes in the last FIFO word.
  // If zero, all bytes in the last FIFO word are valid.
  //
  // Valid only if `last_byte_received` is true and this field is not zero.
  DEF_FIELD(30, 28, valid_bytes_in_last_word_if_not_zero);

  // Number of available words in the FIFO.
  DEF_FIELD(24, 0, word_count);
};

// TX_FIFO_STATUS
//
// This register is not documented in the Hardware register description
// doc.
//
// Experiments show that the register definitions below match the hardware
// behavior.
struct TxFifoStatusRegister : public hwreg::RegisterBase<TxFifoStatusRegister, uint32_t> {
  static auto Get() { return hwreg::RegisterAddr<TxFifoStatusRegister>(0x800); }

  // Number of available words in the FIFO.
  DEF_FIELD(27, 0, word_count);
};

struct IrqStatusRegister : public hwreg::RegisterBase<IrqStatusRegister, uint32_t> {
  DEF_BIT(0, command_done);
  DEF_BIT(1, command_overrun);
  DEF_BIT(2, command_illegal);
  DEF_BIT(3, command_failure);
  DEF_BIT(4, command_cancel);
  DEF_BIT(5, command_abort);
  DEF_BIT(6, timestamp);
  DEF_BIT(7, tx_irq);
  // ...
  DEF_BIT(20, hardware_irq);
  DEF_BIT(21, tx_fifo_not_empty);
  DEF_BIT(22, cts_deassert);  // "IO_DATA_DEASSERT"
  DEF_BIT(23, cts_assert);
  DEF_BIT(24, rx_fifo_read_error);
  DEF_BIT(25, rx_fifo_write_error);
  DEF_BIT(26, rx_fifo_watermark);
  DEF_BIT(27, rx_fifo_last);
  // The secondary sequencer cannot transmit.
  DEF_BIT(28, tx_read_error);
  DEF_BIT(29, tx_write_error);
  DEF_BIT(30, tx_fifo_watermark);
  // In the main sequencer, indicates any bit has been set.
  DEF_BIT(31, sec_irq);

  static auto Get(uint32_t offset) { return hwreg::RegisterAddr<IrqStatusRegister>(offset); }
};

struct MainIrqStatusRegister {
  static auto Get() { return IrqStatusRegister::Get(0x610); }
};

// Enables any bits -- always use Get().FromValue(...) as it is a write register
// All bits will impact what is enabled.
struct MainIrqEnableRegister {
  static auto Get() { return IrqStatusRegister::Get(0x614); }
};

// Clear any bits from the irq status -- always use Get().FromValue(...) as it is a write register
//
// For instance, Get().FromValue(main_irq_status_value).WriteTo(...) will clear
// all status values.
struct MainIrqStatusClearRegister {
  static auto Get() { return IrqStatusRegister::Get(0x618); }
};

// Enables any IRQ bits -- always use Get().FromValue(...) as it is a write register
// Only set bits will be propagated to IrqEnable
struct MainIrqEnableSetRegister {
  static auto Get() { return IrqStatusRegister::Get(0x61c); }
};

// Disables any IRQ bits -- always use Get().FromValue(...) as it is a write register
// Only set bits will be cleared from IrqEnable
struct MainIrqEnableClearRegister {
  static auto Get() { return IrqStatusRegister::Get(0x620); }
};

struct SecondaryIrqStatusRegister {
  static auto Get() { return IrqStatusRegister::Get(0x640); }
};

// Enables any bits -- always use Get().FromValue(...) as it is a write register
// All bits will impact what is enabled.
struct SecondaryIrqEnableRegister {
  static auto Get() { return IrqStatusRegister::Get(0x644); }
};

// Clear any bits from the irq status -- always use Get().FromValue(...) as it is a write register
//
// For instance, Get().FromValue(main_irq_status_value).WriteTo(...) will clear
// all status values.
struct SecondaryIrqStatusClearRegister {
  static auto Get() { return IrqStatusRegister::Get(0x648); }
};

// Enables any IRQ bits -- always use Get().FromValue(...) as it is a write register
// Only set bits will be propagated to IrqEnable
struct SecondaryIrqEnableSetRegister {
  static auto Get() { return IrqStatusRegister::Get(0x64c); }
};

// Disables any IRQ bits -- always use Get().FromValue(...) as it is a write register
// Only set bits will be cleared from IrqEnable
struct SecondaryIrqEnableClearRegister {
  static auto Get() { return IrqStatusRegister::Get(0x650); }
};

struct WatermarkRegister : public hwreg::RegisterBase<WatermarkRegister, uint32_t> {
  DEF_FIELD(5, 0, length);  // Length at which the watermark fires
  static auto Get(uint32_t offset) { return hwreg::RegisterAddr<WatermarkRegister>(offset); }
};

// Module: QUPV3_0_SE0_GENI4_DATA
// QUPV3_0_SE0_GENI_TX_WATERMARK_REG
// GENI TX FIFO Watermark Register
//
// IRQ fires when the length goes below the watermark
// (meaning there is free space in the FIFO to write.)
struct TxWatermarkRegister {
  static auto Get() { return WatermarkRegister::Get(0x80c); }
};

// Module: QUPV3_0_SE0_GENI4_DATA
// QUPV3_0_SE0_GENI_RX_WATERMARK_REG
// GENI RX FIFO Watermark Register
//
// IRQ fires when the length goes above the watermark
// (meaning there is data in the FIFO to write.)
struct RxWatermarkRegister {
  static auto Get() { return WatermarkRegister::Get(0x810); }
};

// Module: QUPV3_0_SE0_GENI4_DATA
// QUPV3_0_SE0_GENI_RX_RFR_WATERMARK_REG
// GENI RX FIFO Ready for Receive Watermark Register
//
// Asserts the hardware condition to continue reading data from the hardware
// when RX FIFO length is less than the watermark (meaning there is free
// space in the FIFO to write).
struct RxReadyForReceiveWatermarkRegister {
  static auto Get() { return WatermarkRegister::Get(0x814); }
};

struct UartTransmitLengthRegister
    : public hwreg::RegisterBase<UartTransmitLengthRegister, uint32_t> {
  DEF_FIELD(23, 0, length);  // Number of words to transmit in the next TX command
  static auto Get() { return hwreg::RegisterAddr<UartTransmitLengthRegister>(0x270); }
};

enum MainCommandOpCode {
  // UART specific op codes
  StartTx = 1,
  StartBreak = 3,
  StopBreak = 5,
};

struct MainCommandRegister : public hwreg::RegisterBase<MainCommandRegister, uint32_t> {
  // 26, 0 not used by uart
  DEF_ENUM_FIELD(MainCommandOpCode, 31, 27, command);
  static auto Get() { return hwreg::RegisterAddr<MainCommandRegister>(0x600); }
};

struct SecondaryCommandRegister : public hwreg::RegisterBase<SecondaryCommandRegister, uint32_t> {
  DEF_BIT(0, enable_search_char);
  DEF_BIT(4, enable_skip_char_with_parity_error);
  DEF_BIT(5, enable_skip_char_with_framing_error);
  DEF_BIT(6, enable_skip_break_char);
  DEF_BIT(27, start_read);
  static auto Get() { return hwreg::RegisterAddr<SecondaryCommandRegister>(0x630); }
};

struct CommandControlRegister : public hwreg::RegisterBase<CommandControlRegister, uint32_t> {
  // 26, 0 not used by uart
  DEF_BIT(0, disable);
  DEF_BIT(1, abort_command);
  DEF_BIT(2, cancel_command);
  static auto Get(uint32_t offset) { return hwreg::RegisterAddr<CommandControlRegister>(offset); }
};

struct MainCommandControlRegister {
  static auto Get() { return CommandControlRegister::Get(0x604); }
};

struct SecondaryCommandControlRegister {
  static auto Get() { return CommandControlRegister::Get(0x634); }
};

struct SerialHwParametersRegister
    : public hwreg::RegisterBase<SerialHwParametersRegister, uint32_t> {
  DEF_BIT(11, fifo_enabled);
  DEF_FIELD(14, 12, async_fifo_depth);
  DEF_FIELD(21, 16, fifo_depth);
  DEF_FIELD(29, 24, fifo_width);
  static auto Get(uint32_t offset) {
    return hwreg::RegisterAddr<SerialHwParametersRegister>(offset);
  }
};

struct TxParametersRegister {
  static auto Get() { return hwreg::RegisterAddr<SerialHwParametersRegister>(0xe24); }
};

struct RxParametersRegister {
  static auto Get() { return hwreg::RegisterAddr<SerialHwParametersRegister>(0xe28); }
};

// This corresponds to the size of the MMIO region from a provided base address.
// In common configurations, each serial engine gets 16kB of register maps.
// Unfortunately, this approach is not ideal for accessing the common QUP SE
// registers for determining hardware version, etc. This slot would envelope all
// engines to reach 0x40000+, so a secondary driver would be needed to simply
// check the GENI FW version register.
static constexpr size_t kIoSlots = 0x4000;

// Common clocking values
// As needed, these can be discovered rather than hard coded.
static constexpr uint32_t kFrequency = 7372800;
static constexpr uint32_t kBaudRate = 115200;
static constexpr uint32_t kOversampling = 16;  // 16 on newer boards, 32 before geni fw 2.5
static constexpr uint32_t kClockRate = kBaudRate * kOversampling;
static constexpr uint32_t kClockDiv = kFrequency / kClockRate;

// FIFO fill watermark in terms of FIFO words (kFifoWidth bytes)
static constexpr uint32_t kTxFifoWatermark = 4;

static constexpr uint32_t kFifoWidth = 4;     // in bytes
static constexpr uint32_t kRxFifoDepth = 16;  // in fifos
static constexpr uint32_t kTxFifoDepth = 16;

class Driver : public DriverBase<Driver, ZBI_KERNEL_DRIVER_GENI_UART, zbi_dcfg_simple_t,
                                 IoRegisterType::kMmio8, kIoSlots> {
 public:
  static constexpr auto kDevicetreeBindings =
      cpp20::to_array<std::string_view>({"qcom,geni-debug-uart"});

  static constexpr std::string_view kConfigName = "geni";

  template <typename... Args>
  explicit Driver(Args&&... args)
      : DriverBase<Driver, ZBI_KERNEL_DRIVER_GENI_UART, zbi_dcfg_simple_t, IoRegisterType::kMmio8,
                   kIoSlots>(std::forward<Args>(args)...) {}

  template <class IoProvider>
  void Init(IoProvider& io) {
    auto tx_hw_params = TxParametersRegister::Get().ReadFrom(io.io());
    tx_fifo_depth_ = tx_hw_params.fifo_depth();
    // Store width in bytes, not bits.
    tx_fifo_width_ = tx_hw_params.fifo_width() >> 3;
    if (tx_fifo_width_ > kFifoWidth) {
      tx_fifo_width_ = kFifoWidth;
    }
    auto rx_hw_params = RxParametersRegister::Get().ReadFrom(io.io());
    rx_fifo_depth_ = rx_hw_params.fifo_depth();
    rx_fifo_width_ = rx_hw_params.fifo_width() >> 3;
    if (rx_fifo_width_ > kFifoWidth) {
      rx_fifo_width_ = kFifoWidth;
    }

    // Note, this is a very lightweight initialization. Without the bootloader
    // preconfiguring the debug UART, this code would need to check for GENI
    // engine activity, abort all pending work, and reset the configs:
    // determining clocking, set packing expectations, etc.

    // Configure the clocks
    auto m_clk = MainClockRegister::Get().FromValue(0);
    m_clk.set_enable(1).set_div(kClockDiv).WriteTo(io.io());
    auto s_clk = SecondaryClockRegister::Get().FromValue(0);
    s_clk.set_enable(1).set_div(kClockDiv).WriteTo(io.io());

    // The bootloader has configured the UART serial engine prior to Fuchsia
    // boot. It has configured the RX and RX RFR (Ready for Receive)
    // watermark registers and started the secondary (read) command on the
    // engine. The FIFO configuration was later reset (?), leaving the serial
    // engine in an unstable state when this driver loads.
    //
    // We have to set the watermark registers with their previous values
    // to make it work properly.
    //
    // TODO(https://fxbug.dev/362847591): Instead of relying on the following
    // reinitialization logic, the driver should reset the serial engine
    // and start the RX procedure from a fresh hardware state.

    // Previous RFR (Ready for Receive) and RX watermark values set by the
    // bootloader driver. Their previous values were not stored in any
    // registers so we have to hard code them.
    static constexpr uint32_t kRxRfrFifoWatermark = kRxFifoDepth - 4;
    static constexpr uint32_t kRxFifoWatermark = kRxFifoDepth - 8;

    // Setup RFR (Ready for Receive) watermark and RX watermark for future
    // interrupt use.
    auto rfr_wm = RxReadyForReceiveWatermarkRegister::Get().FromValue(0);
    rfr_wm.set_length(kRxRfrFifoWatermark).WriteTo(io.io());

    auto rx_wm = RxWatermarkRegister::Get().FromValue(0);
    rx_wm.set_length(kRxFifoWatermark).WriteTo(io.io());

    // Setup TX watermark for future interrupt use.
    auto tx_wm = TxWatermarkRegister::Get().FromValue(0);
    tx_wm.set_length(kTxFifoWatermark).WriteTo(io.io());
  }

  template <class IoProvider>
  uint32_t TxReady(IoProvider& io) {
    // If the engine is busy with the last call, don't bother
    // checking the FIFO status.
    auto geni_status = GeniStatusRegister::Get().ReadFrom(io.io());
    if (geni_status.m_command_active()) {
      return 0;
    }

    auto fifo_count = TxFifoStatusRegister::Get().ReadFrom(io.io()).word_count();
    // Block until it drops below the watermark.
    if (fifo_count >= kTxFifoWatermark) {
      return 0;
    }
    if (fifo_count > tx_fifo_depth_) {
      return 0;
    }
    // Return available bytes to be filled in the FIFO.
    return (tx_fifo_depth_ - fifo_count) * tx_fifo_width_;
  }

  template <class IoProvider, typename It1, typename It2>
  auto Write(IoProvider& io, uint32_t ready_space, It1 it, const It2& end) {
    ptrdiff_t remaining = std::distance(it, end);
    if (remaining <= 0 || remaining >= INT_MAX) {
      return it;
    }
    uint32_t bytes = std::min(static_cast<uint32_t>(remaining), ready_space);
    auto txl = UartTransmitLengthRegister::Get().FromValue(0);
    txl.set_length(bytes).WriteTo(io.io());
    auto m_cmd = MainCommandRegister::Get().FromValue(0);
    m_cmd.set_command(StartTx).WriteTo(io.io());
    do {
      auto tx = TxFifoRegister::Get().FromValue(0);
      uint32_t value = 0;
      uint32_t fifo_len = std::min(tx_fifo_width_, bytes);
      for (uint32_t i = 0; i < fifo_len; ++i, it++, bytes--) {
        // Fill out the value
        value |= (*it & 0xff) << (i * CHAR_BIT);
      }
      tx.set_data(value).WriteTo(io.io());
      arch::DeviceMemoryBarrier();
    } while (it != end && bytes > 0);
    return it;
  }

  template <class IoProvider>
  std::optional<uint8_t> Read(IoProvider& io) {
    if (RxFifoStatusRegister::Get().ReadFrom(io.io()).word_count() == 0) {
      return {};
    }
    return RxFifoRegister::Get().ReadFrom(io.io()).data() & 0xff;
  }

  template <class IoProvider>
  void EnableTxInterrupt(IoProvider& io, bool enable = true) {
    if (enable) {
      auto enable_set = MainIrqEnableSetRegister::Get().FromValue(0);
      enable_set.set_tx_fifo_watermark(1);
      enable_set.set_command_done(1);
      enable_set.set_command_cancel(1);
      enable_set.set_command_abort(1);
      enable_set.WriteTo(io.io());
    } else {
      auto m_en_clear = MainIrqEnableClearRegister::Get().FromValue(0);
      m_en_clear.set_tx_fifo_watermark(1);
      // Only disable the watermark interrupt when a given stream is done.
      // One shots are left active in case there are other blocked writers.
      m_en_clear.WriteTo(io.io());
    }
  }

  template <class IoProvider>
  void EnableRxInterrupt(IoProvider& io, bool enable = true) {
    if (enable) {
      // RX work is handled on the secondary engine, but it is recommended to
      // enable interrupts across both even though they won't be checked or
      // cleared explicitly on the main engine.
      auto m_enable_set = MainIrqEnableSetRegister::Get().FromValue(0);
      m_enable_set.set_rx_fifo_watermark(1);
      m_enable_set.set_rx_fifo_last(1);
      m_enable_set.set_command_cancel(1);
      m_enable_set.set_command_abort(1);
      m_enable_set.WriteTo(io.io());

      auto s_enable_set = SecondaryIrqEnableSetRegister::Get().FromValue(0);
      s_enable_set.set_rx_fifo_watermark(1);
      s_enable_set.set_rx_fifo_last(1);
      s_enable_set.set_command_cancel(1);
      s_enable_set.set_command_abort(1);
      s_enable_set.WriteTo(io.io());
    } else {
      auto m_en_clear = MainIrqEnableClearRegister::Get().FromValue(0);
      m_en_clear.set_rx_fifo_watermark(1);
      m_en_clear.set_rx_fifo_last(1);
      m_en_clear.WriteTo(io.io());

      auto s_en_clear = SecondaryIrqEnableClearRegister::Get().FromValue(0);
      s_en_clear.set_rx_fifo_watermark(1);
      s_en_clear.set_rx_fifo_last(1);
      s_en_clear.WriteTo(io.io());

      auto m_clear = MainIrqStatusClearRegister::Get().FromValue(0);
      m_clear.set_rx_fifo_watermark(1);
      m_clear.set_rx_fifo_last(1);
      m_clear.WriteTo(io.io());

      auto s_clear = SecondaryIrqStatusClearRegister::Get().FromValue(0);
      s_clear.set_rx_fifo_watermark(1);
      s_clear.set_rx_fifo_last(1);
      // These are unused at present.
      // s_clear.set_command_cancel(1);
      // s_clear.set_command_abort(1);
      s_clear.WriteTo(io.io());
    }
  }

  template <class IoProvider, typename EnableInterruptCallback>
  void InitInterrupt(IoProvider& io, EnableInterruptCallback&& enable_interrupt_callback) {
    // Clear any pre-existing enabled interrupts.
    auto m_clear = MainIrqEnableClearRegister::Get().FromValue(0xffffffff);
    auto s_clear = SecondaryIrqEnableClearRegister::Get().FromValue(0xffffffff);
    m_clear.WriteTo(io.io());
    s_clear.WriteTo(io.io());

    // Enable receive interrupts.
    // Transmit interrupts are enabled only when there is a blocked writer.
    EnableRxInterrupt(io, true);
    enable_interrupt_callback();
  }

  template <class IoProvider, typename Lock, typename Waiter, typename Tx, typename Rx>
  void Interrupt(IoProvider& io, Lock& lock, Waiter& waiter, Tx&& tx, Rx&& rx) {
    auto m_status = MainIrqStatusRegister::Get().ReadFrom(io.io());
    auto s_status = SecondaryIrqStatusRegister::Get().ReadFrom(io.io());

    // As this driver is for debug output only, there is no handling of errors,
    // illegal commands, etc.

    // Drain characters in the fifo.
    if (s_status.rx_fifo_watermark() || s_status.rx_fifo_last()) {
      auto rx_fifo_status = RxFifoStatusRegister::Get().ReadFrom(io.io());
      bool ignore_rx = false;

      // If an abort or cancel is raised, then the data should be trashed.
      if (s_status.command_cancel() || s_status.command_abort()) {
        ignore_rx = true;
      }

      // If needed, parity, char hunt, and other status can be checked here
      // using the general purpose IRQs (GP). For debug uart use, they seem
      // unnecessary.

      // Compute the total bytes up front and then we can check on the last
      // fifo if we should expect fewer bytes.
      uint32_t to_drain = rx_fifo_status.word_count() * rx_fifo_width_;
      if (rx_fifo_status.last_byte_received() &&
          rx_fifo_status.valid_bytes_in_last_word_if_not_zero() != 0) {
        // Remove the full fifo size and re-add up to the last byte
        to_drain -= rx_fifo_width_;
        to_drain += rx_fifo_status.valid_bytes_in_last_word_if_not_zero();
      }
      bool rx_disabled = false;
      // Loop once per full fifo (4 bytes) and let the remainder catch the
      // partial().
      while (to_drain > 0) {
        uint32_t fifo_len = std::min(rx_fifo_width_, to_drain);
        uint32_t value = RxFifoRegister::Get().ReadFrom(io.io()).data();
        to_drain -= fifo_len;
        for (uint32_t c = 0; c < fifo_len && !rx_disabled; ++c) {
          auto rx_irq = RxInterrupt(
              lock, [&]() { return ignore_rx ? 0 : (value >> (c * CHAR_BIT)) & 0xff; },
              [&]() {
                // If the buffer is full, disable the receive interrupt instead
                // and stop checking.
                EnableRxInterrupt(io, false);
                rx_disabled = true;
              });

          rx(rx_irq);
        }
      }
    }

    // If any of the conditions below have been raised, attempting to transmit is ok.
    if (m_status.tx_fifo_watermark() || m_status.command_done() || m_status.command_cancel() ||
        m_status.command_abort()) {
      auto tx_irq = TxInterrupt(lock, waiter, [&]() { EnableTxInterrupt(io, false); });
      tx(tx_irq);
    }

    // Clear the flags we're handling.
    auto m_clear = MainIrqStatusClearRegister::Get().FromValue(m_status.reg_value());
    m_clear.WriteTo(io.io());
    auto s_clear = SecondaryIrqStatusClearRegister::Get().FromValue(s_status.reg_value());
    s_clear.WriteTo(io.io());
  }

 private:
  uint32_t rx_fifo_depth_ = kRxFifoDepth;
  uint32_t tx_fifo_depth_ = kTxFifoDepth;
  uint32_t rx_fifo_width_ = kFifoWidth;
  uint32_t tx_fifo_width_ = kFifoWidth;
};

}  // namespace uart::geni

#endif  // LIB_UART_GENI_H_
