// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/serial/drivers/aml-uart/aml-uart.h"

#ifdef DFV1
#include <lib/ddk/debug.h>  // nogncheck
#else
#include <lib/driver/compat/cpp/logging.h>  // nogncheck
#endif

#include <lib/zx/clock.h>
#include <threads.h>
#include <zircon/syscalls-next.h>
#include <zircon/threads.h>

#include <fbl/auto_lock.h>

#include "src/devices/serial/drivers/aml-uart/registers.h"

namespace fhsi = fuchsia_hardware_serialimpl;

namespace serial {
namespace internal {

fit::closure DriverTransportReadOperation::MakeCallback(zx_status_t status, void* buf, size_t len) {
  return fit::closure(
      [arena = std::move(arena_), completer = std::move(completer_), status, buf, len]() mutable {
        if (status == ZX_OK) {
          completer.buffer(arena).ReplySuccess(
              fidl::VectorView<uint8_t>::FromExternal(static_cast<uint8_t*>(buf), len));
        } else {
          completer.buffer(arena).ReplyError(status);
        }
      });
}

fit::closure DriverTransportWriteOperation::MakeCallback(zx_status_t status) {
  return fit::closure(
      [arena = std::move(arena_), completer = std::move(completer_), status]() mutable {
        if (status == ZX_OK) {
          completer.buffer(arena).ReplySuccess();
        } else {
          completer.buffer(arena).ReplyError(status);
        }
      });
}

}  // namespace internal

AmlUart::AmlUart(fdf::PDev pdev,
                 const fuchsia_hardware_serial::wire::SerialPortInfo& serial_port_info,
                 fdf::MmioBuffer mmio, fdf::UnownedSynchronizedDispatcher irq_dispatcher,
                 std::optional<fdf::UnownedSynchronizedDispatcher> timer_dispatcher,
                 bool power_control_enabled,
                 std::optional<fidl::ClientEnd<fuchsia_power_system::ActivityGovernor>> sag)
    : pdev_(std::move(pdev)),
      serial_port_info_(serial_port_info),
      mmio_(std::move(mmio)),
      irq_dispatcher_(std::move(irq_dispatcher)),
      power_control_enabled_(power_control_enabled) {
  if (timer_dispatcher != std::nullopt) {
    timer_dispatcher_ = std::move(*timer_dispatcher);
    // Use ZX_TIMER_SLACK_LATE so that the timer will never fire earlier than the deadline.
    zx::timer::create(ZX_TIMER_SLACK_LATE, ZX_CLOCK_MONOTONIC, &lease_timer_);
    timer_waiter_.set_object(lease_timer_.get());
    timer_waiter_.set_trigger(ZX_TIMER_SIGNALED);
  }

  if (sag != std::nullopt) {
    sag_.Bind(std::move(*sag));
    this->sag_available_ = true;
  }
}

constexpr auto kMinBaudRate = 2;

bool AmlUart::Readable() { return !Status::Get().ReadFrom(&mmio_).rx_empty(); }
bool AmlUart::Writable() { return !Status::Get().ReadFrom(&mmio_).tx_full(); }

void AmlUart::GetInfo(fdf::Arena& arena, GetInfoCompleter::Sync& completer) {
  completer.buffer(arena).ReplySuccess(serial_port_info_);
}

zx_status_t AmlUart::Config(uint32_t baud_rate, uint32_t flags) {
  // Control register is determined completely by this logic, so start with a clean slate.
  if (baud_rate < kMinBaudRate) {
    return ZX_ERR_INVALID_ARGS;
  }
  auto ctrl = Control::Get().FromValue(0);

  if ((flags & fhsi::kSerialSetBaudRateOnly) == 0) {
    switch (flags & fhsi::kSerialDataBitsMask) {
      case fhsi::kSerialDataBits5:
        ctrl.set_xmit_len(Control::kXmitLength5);
        break;
      case fhsi::kSerialDataBits6:
        ctrl.set_xmit_len(Control::kXmitLength6);
        break;
      case fhsi::kSerialDataBits7:
        ctrl.set_xmit_len(Control::kXmitLength7);
        break;
      case fhsi::kSerialDataBits8:
        ctrl.set_xmit_len(Control::kXmitLength8);
        break;
      default:
        return ZX_ERR_INVALID_ARGS;
    }

    switch (flags & fhsi::kSerialStopBitsMask) {
      case fhsi::kSerialStopBits1:
        ctrl.set_stop_len(Control::kStopLen1);
        break;
      case fhsi::kSerialStopBits2:
        ctrl.set_stop_len(Control::kStopLen2);
        break;
      default:
        return ZX_ERR_INVALID_ARGS;
    }

    switch (flags & fhsi::kSerialParityMask) {
      case fhsi::kSerialParityNone:
        ctrl.set_parity(Control::kParityNone);
        break;
      case fhsi::kSerialParityEven:
        ctrl.set_parity(Control::kParityEven);
        break;
      case fhsi::kSerialParityOdd:
        ctrl.set_parity(Control::kParityOdd);
        break;
      default:
        return ZX_ERR_INVALID_ARGS;
    }

    switch (flags & fhsi::kSerialFlowCtrlMask) {
      case fhsi::kSerialFlowCtrlNone:
        ctrl.set_two_wire(1);
        break;
      case fhsi::kSerialFlowCtrlCtsRts:
        // CTS/RTS is on by default
        break;
      default:
        return ZX_ERR_INVALID_ARGS;
    }
  }

  // Configure baud rate based on crystal clock speed.
  // See meson_uart_change_speed() in drivers/amlogic/uart/uart/meson_uart.c.
  constexpr uint32_t kCrystalClockSpeed = 24000000;
  uint32_t baud_bits = (kCrystalClockSpeed / 3) / baud_rate - 1;
  if (baud_bits & (~AML_UART_REG5_NEW_BAUD_RATE_MASK)) {
    zxlogf(ERROR, "%s: baud rate %u too large", __func__, baud_rate);
    return ZX_ERR_OUT_OF_RANGE;
  }
  auto baud = Reg5::Get()
                  .FromValue(0)
                  .set_new_baud_rate(baud_bits)
                  .set_use_xtal_clk(1)
                  .set_use_new_baud_rate(1);

  fbl::AutoLock al(&enable_lock_);

  if ((flags & fhsi::kSerialSetBaudRateOnly) == 0) {
    // Invert our RTS if we are we are not enabled and configured for flow control.
    if (!enabled_ && (ctrl.two_wire() == 0)) {
      ctrl.set_inv_rts(1);
    }
    ctrl.WriteTo(&mmio_);
  }

  baud.WriteTo(&mmio_);

  return ZX_OK;
}

void AmlUart::EnableLocked(bool enable) {
  auto ctrl = Control::Get().ReadFrom(&mmio_);

  if (enable) {
    // Reset the port.
    ctrl.set_rst_rx(1).set_rst_tx(1).set_clear_error(1).WriteTo(&mmio_);

    ctrl.set_rst_rx(0).set_rst_tx(0).set_clear_error(0).WriteTo(&mmio_);

    // Enable rx and tx.
    ctrl.set_tx_enable(1)
        .set_rx_enable(1)
        .set_tx_interrupt_enable(1)
        .set_rx_interrupt_enable(1)
        // Clear our RTS.
        .set_inv_rts(0)
        .WriteTo(&mmio_);

    // Set interrupt thresholds.
    // Generate interrupt if TX buffer drops below half full.
    constexpr uint32_t kTransmitIrqCount = 32;
    // Generate interrupt as soon as we receive any data.
    constexpr uint32_t kRecieveIrqCount = 1;
    Misc::Get()
        .FromValue(0)
        .set_xmit_irq_count(kTransmitIrqCount)
        .set_recv_irq_count(kRecieveIrqCount)
        .WriteTo(&mmio_);
  } else {
    ctrl.set_tx_enable(0)
        .set_rx_enable(0)
        // Invert our RTS if we are configured for flow control.
        .set_inv_rts(!ctrl.two_wire())
        .WriteTo(&mmio_);
  }
}

void AmlUart::HandleTXRaceForTest() {
  {
    fbl::AutoLock al(&enable_lock_);
    EnableLocked(true);
  }
  Writable();
  HandleTX();
  HandleTX();
}

void AmlUart::HandleRXRaceForTest() {
  {
    fbl::AutoLock al(&enable_lock_);
    EnableLocked(true);
  }
  Readable();
  HandleRX();
  HandleRX();
}

void AmlUart::InjectTimerForTest(zx_handle_t handle) {
  lease_timer_.reset(handle);
  timer_waiter_.set_object(lease_timer_.get());
  timer_waiter_.set_trigger(ZX_TIMER_SIGNALED);
}

zx_status_t AmlUart::Enable(bool enable) {
  fbl::AutoLock al(&enable_lock_);

  if (enable && !enabled_) {
    if (power_control_enabled_) {
      zx::result irq = pdev_.GetInterrupt(0, ZX_INTERRUPT_WAKE_VECTOR);
      if (irq.is_error()) {
        zxlogf(ERROR, "Failed to get pdev: %s", irq.status_string());
        return irq.status_value();
      }
      irq_ = std::move(irq.value());
    } else {
      zx::result irq = pdev_.GetInterrupt(0, 0);
      if (irq.is_error()) {
        zxlogf(ERROR, "Failed to get pdev: %s", irq.status_string());
        return irq.status_value();
      }
      irq_ = std::move(irq.value());
    }

    EnableLocked(true);

    irq_handler_.set_object(irq_.get());
    irq_handler_.Begin(irq_dispatcher_->async_dispatcher());
  } else if (!enable && enabled_) {
    irq_handler_.Cancel();
    EnableLocked(false);
  }

  enabled_ = enable;

  return ZX_OK;
}

void AmlUart::CancelAll(fdf::Arena& arena, CancelAllCompleter::Sync& completer) {
  {
    fbl::AutoLock read_lock(&read_lock_);
    if (read_operation_) {
      auto cb = MakeReadCallbackLocked(ZX_ERR_CANCELED, nullptr, 0);
      read_lock.release();
      cb();
    }
  }
  {
    fbl::AutoLock write_lock(&write_lock_);
    if (write_operation_) {
      auto cb = MakeWriteCallbackLocked(ZX_ERR_CANCELED);
      write_lock.release();
      cb();
    }
  }

  completer.buffer(arena).Reply();
}

// Handles receiviung data into the buffer and calling the read callback when complete.
// Does nothing if read_pending_ is false.
void AmlUart::HandleRX() {
  fbl::AutoLock lock(&read_lock_);
  if (!read_operation_) {
    return;
  }
  unsigned char buf[128];
  size_t length = 128;
  auto* bufptr = static_cast<uint8_t*>(buf);
  const uint8_t* const end = bufptr + length;
  while (bufptr < end && Readable()) {
    uint32_t val = mmio_.Read32(AML_UART_RFIFO);
    *bufptr++ = static_cast<uint8_t>(val);
  }

  const size_t read = reinterpret_cast<uintptr_t>(bufptr) - reinterpret_cast<uintptr_t>(buf);
  if (read == 0) {
    return;
  }
  // Some bytes were read.  The client must queue another read to get any data.
  auto cb = MakeReadCallbackLocked(ZX_OK, buf, read);
  lock.release();
  cb();
}

// Handles transmitting the data in write_buffer_ until it is completely written.
// Does nothing if write_pending_ is not true.
void AmlUart::HandleTX() {
  fbl::AutoLock lock(&write_lock_);
  if (!write_operation_) {
    return;
  }
  const auto* bufptr = static_cast<const uint8_t*>(write_buffer_);
  const uint8_t* const end = bufptr + write_size_;
  while (bufptr < end && Writable()) {
    mmio_.Write32(*bufptr++, AML_UART_WFIFO);
  }

  const size_t written =
      reinterpret_cast<uintptr_t>(bufptr) - reinterpret_cast<uintptr_t>(write_buffer_);
  write_size_ -= written;
  write_buffer_ += written;
  if (!write_size_) {
    // The write has completed, notify the client.
    auto cb = MakeWriteCallbackLocked(ZX_OK);
    lock.release();
    cb();
  }
}

fit::closure AmlUart::MakeReadCallbackLocked(zx_status_t status, void* buf, size_t len) {
  if (read_operation_) {
    auto callback = read_operation_->MakeCallback(status, buf, len);
    read_operation_.reset();
    return callback;
  }

  ZX_PANIC("AmlUart::MakeReadCallbackLocked invalid state. No active Read operation.");
}

fit::closure AmlUart::MakeWriteCallbackLocked(zx_status_t status) {
  if (write_operation_) {
    auto callback = write_operation_->MakeCallback(status);
    write_operation_.reset();
    return callback;
  }

  ZX_PANIC("AmlUart::MakeWriteCallbackLocked invalid state. No active Write operation.");
}

void AmlUart::Config(ConfigRequestView request, fdf::Arena& arena,
                     ConfigCompleter::Sync& completer) {
  zx_status_t status = Config(request->baud_rate, request->flags);
  if (status == ZX_OK) {
    completer.buffer(arena).ReplySuccess();
  } else {
    completer.buffer(arena).ReplyError(status);
  }
}

void AmlUart::Enable(EnableRequestView request, fdf::Arena& arena,
                     EnableCompleter::Sync& completer) {
  zx_status_t status = Enable(request->enable);
  if (status == ZX_OK) {
    completer.buffer(arena).ReplySuccess();
  } else {
    completer.buffer(arena).ReplyError(status);
  }
}

void AmlUart::Read(fdf::Arena& arena, ReadCompleter::Sync& completer) {
  fbl::AutoLock lock(&read_lock_);
  if (read_operation_) {
    lock.release();
    completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
    return;
  }
  read_operation_.emplace(std::move(arena), completer.ToAsync());
  lock.release();
  HandleRX();
}

void AmlUart::Write(WriteRequestView request, fdf::Arena& arena, WriteCompleter::Sync& completer) {
  fbl::AutoLock lock(&write_lock_);
  if (write_operation_) {
    lock.release();
    completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
    return;
  }
  write_buffer_ = request->data.data();
  write_size_ = request->data.count();
  write_operation_.emplace(std::move(arena), completer.ToAsync());
  lock.release();
  HandleTX();
}

void AmlUart::handle_unknown_method(
    fidl::UnknownMethodMetadata<fuchsia_hardware_serialimpl::Device> metadata,
    fidl::UnknownMethodCompleter::Sync& completer) {
  zxlogf(WARNING, "handle_unknown_method in fuchsia_hardware_serialimpl::Device server.");
}

void AmlUart::HandleIrq(async_dispatcher_t* dispatcher, async::IrqBase* irq, zx_status_t status,
                        const zx_packet_interrupt_t* interrupt) {
  if (status != ZX_OK) {
    return;
  }
  // If sag is not available, it means that power management is not enabled for this driver,
  // the driver will not take a wake lease and set timer in this case.
  if (this->sag_available_) {
    fbl::AutoLock lock(&timer_lock_);

    if (sag_.is_valid() && !token_) {
      // Take a wake lease.
      fidl::Request<::fuchsia_power_system::ActivityGovernor::TakeWakeLease> request;
      request.name("uart");
      auto activity_governor_result = sag_->TakeWakeLease(request);
      if (activity_governor_result.is_error()) {
        zxlogf(ERROR, "Failed to take wake lease: %s",
               activity_governor_result.error_value().FormatDescription().c_str());
        return;
      }
      token_ = std::move(activity_governor_result.value().token());
    }

    timeout_ = zx::deadline_after(zx::msec(kPowerLeaseTimeoutMs));
    // Must set the new timeout before the wait on dispatcher begins, otherwise if there was a
    // previous timer that has been fired, the new wait on dispatcher will capture the asserted
    // signal again. Also we don't need to worry about missing the timeout signal from the new timer
    // set, because the timeout signal will persist after timer fires.
    lease_timer_.set(timeout_, zx::duration());
    timer_waiter_.Begin(timer_dispatcher_->async_dispatcher());
  }

  auto uart_status = Status::Get().ReadFrom(&mmio_);
  if (!uart_status.rx_empty()) {
    HandleRX();
  }
  if (!uart_status.tx_full()) {
    HandleTX();
  }

  irq_.ack();
}

void AmlUart::HandleLeaseTimer(async_dispatcher_t* dispatcher, async::WaitBase* wait,
                               zx_status_t status, const zx_packet_signal_t* signal) {
  fbl::AutoLock lock(&timer_lock_);
  if (status == ZX_ERR_CANCELED) {
    // Do nothing if the handler is triggered by the destroy of the dispatcher.
    return;
  }
  if (status != ZX_OK) {
    zxlogf(ERROR, "Timer fired with an error: %s", zx_status_get_string(status));
    return;
  }

  if (zx::clock::get_monotonic() < timeout_) {
    // If the current time is earlier than timeout, it means that this handler is triggered after
    // |HandleIrq| holds the lock, and when this handler get the lock, the timer has been reset, so
    // don't drop the lease in this case.
    zxlogf(INFO,
           "Timer has been reset by a new interrupt, this handler is out-of-date, so do nothing.");
    return;
  }

  // Release the wake lease.
  token_.reset();
}

}  // namespace serial
