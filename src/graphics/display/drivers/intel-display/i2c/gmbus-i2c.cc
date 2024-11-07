// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/intel-display/i2c/gmbus-i2c.h"

#include <threads.h>

#include <fbl/auto_lock.h>

#include "src/graphics/display/drivers/intel-display/i2c/gmbus-gpio.h"
#include "src/graphics/display/drivers/intel-display/registers-gmbus.h"
#include "src/graphics/display/lib/driver-utils/poll-until.h"

namespace intel_display {

namespace {

void WriteGMBusData(fdf::MmioBuffer* mmio_space, const uint8_t* buf, uint32_t size, uint32_t idx) {
  if (idx >= size) {
    return;
  }
  cpp20::span<const uint8_t> data(buf + idx, std::min(4u, size - idx));
  registers::GMBusData::Get().FromValue(0).set_data(data).WriteTo(mmio_space);
}

void ReadGMBusData(fdf::MmioBuffer* mmio_space, uint8_t* buf, uint32_t size, uint32_t idx) {
  int cur_byte = 0;
  auto bytes = registers::GMBusData::Get().ReadFrom(mmio_space).data();
  while (idx < size && cur_byte < 4) {
    buf[idx++] = bytes[cur_byte++];
  }
}

static constexpr uint8_t kDdcSegmentAddress = 0x30;
static constexpr uint8_t kDdcDataAddress = 0x50;
static constexpr uint8_t kI2cClockUs = 10;  // 100 kHz

// For bit banging i2c over the gpio pins
bool i2c_scl(fdf::MmioBuffer* mmio_space, const GpioPort& gpio_port, bool hi) {
  auto gpio_pin_pair_control = registers::GpioPinPairControl::GetForPort(gpio_port).FromValue(0);

  if (!hi) {
    gpio_pin_pair_control.set_clock_direction_is_output(true);
    gpio_pin_pair_control.set_write_clock_output(true);
  }
  gpio_pin_pair_control.set_write_clock_direction_is_output(true);

  gpio_pin_pair_control.WriteTo(mmio_space);
  gpio_pin_pair_control.ReadFrom(mmio_space);  // Posting read

  // Handle the case where something on the bus is holding the clock
  // low. Timeout after 1ms.
  if (hi) {
    int count = 0;
    do {
      if (count != 0) {
        zx_nanosleep(zx_deadline_after(ZX_USEC(kI2cClockUs)));
      }
      gpio_pin_pair_control.ReadFrom(mmio_space);
    } while (count++ < 100 && hi != gpio_pin_pair_control.clock_input());
    if (hi != gpio_pin_pair_control.clock_input()) {
      return false;
    }
  }
  zx_nanosleep(zx_deadline_after(ZX_USEC(kI2cClockUs / 2)));
  return true;
}

// For bit banging i2c over the gpio pins
void i2c_sda(fdf::MmioBuffer* mmio_space, const GpioPort& gpio_port, bool hi) {
  auto gpio_pin_pair_control = registers::GpioPinPairControl::GetForPort(gpio_port).FromValue(0);

  if (!hi) {
    gpio_pin_pair_control.set_data_direction_is_output(true);
    gpio_pin_pair_control.set_write_data_output(true);
  }
  gpio_pin_pair_control.set_write_data_direction_is_output(true);

  gpio_pin_pair_control.WriteTo(mmio_space);
  gpio_pin_pair_control.ReadFrom(mmio_space);  // Posting read

  zx_nanosleep(zx_deadline_after(ZX_USEC(kI2cClockUs / 2)));
}

// For bit banging i2c over the gpio pins
bool i2c_send_byte(fdf::MmioBuffer* mmio_space, const GpioPort& gpio_port, uint8_t byte) {
  // Set the bits from MSB to LSB
  for (int i = 7; i >= 0; i--) {
    i2c_sda(mmio_space, gpio_port, (byte >> i) & 0x1);

    i2c_scl(mmio_space, gpio_port, 1);

    // Leave the data line where it is for the rest of the cycle
    zx_nanosleep(zx_deadline_after(ZX_USEC(kI2cClockUs / 2)));

    i2c_scl(mmio_space, gpio_port, 0);
  }

  // Release the data line and check for an ack
  i2c_sda(mmio_space, gpio_port, 1);
  i2c_scl(mmio_space, gpio_port, 1);

  bool ack =
      !registers::GpioPinPairControl::GetForPort(gpio_port).ReadFrom(mmio_space).data_input();

  // Sleep for the rest of the cycle
  zx_nanosleep(zx_deadline_after(ZX_USEC(kI2cClockUs / 2)));

  i2c_scl(mmio_space, gpio_port, 0);

  return ack;
}

}  // namespace

// Per the GMBUS Controller Programming Interface section of the Intel docs, GMBUS does not
// directly support segment pointer addressing. Instead, the segment pointer needs to be
// set by bit-banging the GPIO pins.
bool GMBusI2c::SetDdcSegment(uint8_t segment_num) {
  ZX_ASSERT(gpio_port_.has_value());

  // Reset the clock and data lines
  i2c_scl(mmio_space_, *gpio_port_, 0);
  i2c_sda(mmio_space_, *gpio_port_, 0);

  if (!i2c_scl(mmio_space_, *gpio_port_, 1)) {
    return false;
  }
  i2c_sda(mmio_space_, *gpio_port_, 1);
  // Wait for the rest of the cycle
  zx_nanosleep(zx_deadline_after(ZX_USEC(kI2cClockUs / 2)));

  // Send a start condition
  i2c_sda(mmio_space_, *gpio_port_, 0);
  i2c_scl(mmio_space_, *gpio_port_, 0);

  // Send the segment register index and the segment number
  uint8_t segment_write_command = kDdcSegmentAddress << 1;
  if (!i2c_send_byte(mmio_space_, *gpio_port_, segment_write_command) ||
      !i2c_send_byte(mmio_space_, *gpio_port_, segment_num)) {
    return false;
  }

  // Set the data and clock lines high to prepare for the GMBus start
  i2c_sda(mmio_space_, *gpio_port_, 1);
  return i2c_scl(mmio_space_, *gpio_port_, 1);
}

static constexpr size_t kMaxTransferSize = 255;
zx_status_t GMBusI2c::I2cImplGetMaxTransferSize(uint64_t* out_size) {
  *out_size = kMaxTransferSize;
  return ZX_OK;
}

zx_status_t GMBusI2c::I2cImplSetBitrate(uint32_t bitrate) {
  // no-op for now
  return ZX_OK;
}

zx_status_t GMBusI2c::I2cImplTransact(const i2c_impl_op_t* ops, size_t size) {
  ZX_ASSERT(gmbus_pin_pair_.has_value());
  ZX_ASSERT(gpio_port_.has_value());

  for (unsigned i = 0; i < size; i++) {
    if (ops[i].data_size > kMaxTransferSize) {
      return ZX_ERR_INVALID_ARGS;
    }
  }
  if (!ops[size - 1].stop) {
    return ZX_ERR_INVALID_ARGS;
  }

  // The GMBus register is a limited interface to the i2c bus - it doesn't support complex
  // transactions like setting the E-DDC segment. For now, providing a special-case interface
  // for reading the E-DDC is good enough.
  fbl::AutoLock lock(&lock_);
  zx_status_t fail_res = ZX_ERR_IO;
  bool gmbus_set = false;
  for (unsigned i = 0; i < size; i++) {
    const i2c_impl_op_t* op = ops + i;
    if (op->address == kDdcSegmentAddress && !op->is_read && op->data_size == 1) {
      registers::GMBusClockPortSelect::Get().FromValue(0).WriteTo(mmio_space_);
      gmbus_set = false;
      if (!SetDdcSegment(*static_cast<uint8_t*>(op->data_buffer))) {
        goto fail;
      }
    } else if (op->address == kDdcDataAddress) {
      if (!gmbus_set) {
        auto gmbus_clock_port_select = registers::GMBusClockPortSelect::Get().FromValue(0);
        gmbus_clock_port_select.SetPinPair(*gmbus_pin_pair_).WriteTo(mmio_space_);

        gmbus_set = true;
      }

      uint8_t* buf = static_cast<uint8_t*>(op->data_buffer);
      uint8_t len = static_cast<uint8_t>(op->data_size);
      if (op->is_read ? GMBusRead(kDdcDataAddress, buf, len)
                      : GMBusWrite(kDdcDataAddress, buf, len)) {
        // Alias `mmio_space_` to aid Clang's thread safety analyzer, which
        // can't reason about closure scopes. The type system helps ensure
        // thread-safety, because the scope of the alias is included in the
        // scope of the AutoLock.
        fdf::MmioBuffer& mmio_space = *mmio_space_;

        if (!display::PollUntil(
                [&]() {
                  return registers::GMBusControllerStatus::Get().ReadFrom(&mmio_space).is_waiting();
                },
                zx::msec(1), 10)) {
          FDF_LOG(TRACE, "Transition to wait phase timed out");
          goto fail;
        }
      } else {
        goto fail;
      }
    } else {
      fail_res = ZX_ERR_NOT_SUPPORTED;
      goto fail;
    }

    if (op->stop) {
      if (!I2cFinish()) {
        goto fail;
      }
      gmbus_set = false;
    }
  }

  return ZX_OK;
fail:
  if (!I2cClearNack()) {
    FDF_LOG(TRACE, "Failed to clear nack");
  }
  return fail_res;
}

bool GMBusI2c::GMBusWrite(uint8_t addr, const uint8_t* buf, uint8_t size) {
  unsigned idx = 0;
  WriteGMBusData(mmio_space_, buf, size, idx);
  idx += 4;

  auto gmbus_command = registers::GMBusCommand::Get().FromValue(0);
  gmbus_command.set_software_ready(true);
  gmbus_command.set_wait_state_enabled(true);
  gmbus_command.set_total_byte_count(size);
  gmbus_command.set_target_address(addr);
  gmbus_command.WriteTo(mmio_space_);

  while (idx < size) {
    if (!I2cWaitForHwReady()) {
      return false;
    }

    WriteGMBusData(mmio_space_, buf, size, idx);
    idx += 4;
  }
  // One more wait to ensure we're ready when we leave the function
  return I2cWaitForHwReady();
}

bool GMBusI2c::GMBusRead(uint8_t addr, uint8_t* buf, uint8_t size) {
  auto gmbus_command = registers::GMBusCommand::Get().FromValue(0);
  gmbus_command.set_software_ready(true);
  gmbus_command.set_wait_state_enabled(true);
  gmbus_command.set_total_byte_count(size);
  gmbus_command.set_target_address(addr);
  gmbus_command.set_is_read_transaction(true);
  gmbus_command.WriteTo(mmio_space_);

  unsigned idx = 0;
  while (idx < size) {
    if (!I2cWaitForHwReady()) {
      return false;
    }

    ReadGMBusData(mmio_space_, buf, size, idx);
    idx += 4;
  }

  return true;
}

bool GMBusI2c::I2cFinish() {
  auto gmbus_command = registers::GMBusCommand::Get().FromValue(0);
  gmbus_command.set_stop_generated(true);
  gmbus_command.set_software_ready(true);
  gmbus_command.WriteTo(mmio_space_);

  // Alias `mmio_space_` to aid Clang's thread safety analyzer, which can't
  // reason about closure scopes. The type system still helps ensure
  // thread-safety, because the scope of the alias is smaller than the method
  // scope, and the method is guaranteed to hold the lock.
  fdf::MmioBuffer& mmio_space = *mmio_space_;
  bool idle = display::PollUntil(
      [&] { return !registers::GMBusControllerStatus::Get().ReadFrom(&mmio_space).is_active(); },
      zx::msec(1), 100);

  auto gmbus_clock_port_select = registers::GMBusClockPortSelect::Get().FromValue(0);
  gmbus_clock_port_select.set_pin_pair_select(0);
  gmbus_clock_port_select.WriteTo(mmio_space_);

  if (!idle) {
    FDF_LOG(TRACE, "hdmi: GMBus i2c failed to go idle");
  }
  return idle;
}

bool GMBusI2c::I2cWaitForHwReady() {
  auto gmbus_controller_status = registers::GMBusControllerStatus::Get().FromValue(0);

  // Alias `mmio_space_` to aid Clang's thread safety analyzer, which can't
  // reason about closure scopes. The type system still helps ensure
  // thread-safety, because the scope of the alias is smaller than the method
  // scope, and the method is guaranteed to hold the lock.
  fdf::MmioBuffer& mmio_space = *mmio_space_;

  if (!display::PollUntil(
          [&] {
            gmbus_controller_status.ReadFrom(&mmio_space);
            return gmbus_controller_status.nack_occurred() || gmbus_controller_status.is_ready();
          },
          zx::msec(1), 50)) {
    FDF_LOG(TRACE, "hdmi: GMBus i2c wait for hwready timeout");
    return false;
  }
  if (gmbus_controller_status.nack_occurred()) {
    FDF_LOG(TRACE, "hdmi: GMBus i2c got nack");
    return false;
  }
  return true;
}

bool GMBusI2c::I2cClearNack() {
  I2cFinish();

  // Alias `mmio_space_` to aid Clang's thread safety analyzer, which can't
  // reason about closure scopes. The type system still helps ensure
  // thread-safety, because the scope of the alias is smaller than the method
  // scope, and the method is guaranteed to hold the lock.
  fdf::MmioBuffer& mmio_space = *mmio_space_;

  if (!display::PollUntil(
          [&] {
            return !registers::GMBusControllerStatus::Get().ReadFrom(&mmio_space).is_active();
          },
          zx::msec(1), 10)) {
    FDF_LOG(TRACE, "hdmi: GMBus i2c failed to clear active nack");
    return false;
  }

  // Set/clear sw clear int to reset the bus
  auto gmbus_command = registers::GMBusCommand::Get().FromValue(0);
  gmbus_command.set_software_clear_interrupt(true);
  gmbus_command.WriteTo(mmio_space_);
  gmbus_command.set_software_clear_interrupt(false);
  gmbus_command.WriteTo(mmio_space_);

  // Reset GMBus0
  auto gmbus_clock_port_select = registers::GMBusClockPortSelect::Get().FromValue(0);
  gmbus_clock_port_select.WriteTo(mmio_space_);

  return true;
}

GMBusI2c::GMBusI2c(DdiId ddi_id, registers::Platform platform, fdf::MmioBuffer* mmio_space)
    : gmbus_pin_pair_(GMBusPinPair::GetForDdi(ddi_id, platform)),
      gpio_port_(GpioPort::GetForDdi(ddi_id, platform)),
      mmio_space_(mmio_space) {
  ZX_ASSERT(mtx_init(&lock_, mtx_plain) == thrd_success);
}

}  // namespace intel_display
