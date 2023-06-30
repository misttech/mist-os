// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "gt92xx.h"

#include <endian.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/platform-defs.h>
#include <lib/ddk/trace/event.h>
#include <lib/fit/defer.h>
#include <lib/zx/profile.h>
#include <lib/zx/thread.h>
#include <lib/zx/time.h>
#include <zircon/status.h>
#include <zircon/syscalls.h>
#include <zircon/threads.h>

#include <iterator>
#include <utility>

#include <fbl/algorithm.h>
#include <fbl/auto_lock.h>
#include <fbl/vector.h>

namespace goodix {
// clang-format off
// Configuration data
// first two bytes contain starting register address (part of i2c transaction)
fbl::Vector<uint8_t> Gt92xxDevice::GetConfData() {
  return {GT_REG_CONFIG_DATA >> 8,
              GT_REG_CONFIG_DATA & 0xff,
              0x5f, 0x00, 0x04, 0x58, 0x02, 0x05, 0xbd, 0xc0,
              0x00, 0x08, 0x1e, 0x05, 0x50, 0x32, 0x00, 0x0b,
              0x00, 0x00, 0x00, 0x00, 0x40, 0x12, 0x00, 0x17,
              0x17, 0x19, 0x12, 0x8d, 0x2d, 0x0f, 0x3f, 0x41,
              0xb2, 0x04, 0x00, 0x00, 0x00, 0xbc, 0x03, 0x1d,
              0x1e, 0x80, 0x01, 0x00, 0x14, 0x46, 0x00, 0x00,
              0x00, 0x00, 0x00, 0x37, 0x55, 0x8f, 0xc5, 0x02,
              0x07, 0x11, 0x00, 0x04, 0x8a, 0x39, 0x00, 0x81,
              0x3e, 0x00, 0x78, 0x44, 0x00, 0x71, 0x4a, 0x00,
              0x6a, 0x51, 0x00, 0x6a, 0x00, 0x00, 0x00, 0x00,
              0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
              0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
              0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
              0x00, 0x00, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00,
              0x1c, 0x1a, 0x18, 0x16, 0x14, 0x12, 0x10, 0x0e,
              0x0c, 0x0a, 0x08, 0x06, 0x04, 0x02, 0x00, 0x00,
              0xff, 0xff, 0x1f, 0xe7, 0xff, 0xff, 0xff, 0x0f,
              0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x2a, 0x29,
              0x28, 0x27, 0x26, 0x25, 0x24, 0x23, 0x22, 0x21,
              0x20, 0x1f, 0x1e, 0x0c, 0x0b, 0x0a, 0x09, 0x08,
              0x07, 0x06, 0x05, 0x04, 0x03, 0x02, 0x01, 0x00,
              0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
              0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
              0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
              0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
              0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
              0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
              0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
              0x00, 0x00, 0x6e, 0x01 };
}
// clang-format on

int Gt92xxDevice::Thread() {
  zx_status_t status;
  zx::time timestamp;
  zxlogf(INFO, "gt92xx: entering irq thread");
  while (true) {
    status = irq_.wait(&timestamp);
    if (!running_.load()) {
      return ZX_OK;
    }
    if (status != ZX_OK) {
      zxlogf(ERROR, "gt92xx: Interrupt error %d", status);
    }
    TRACE_DURATION("input", "Gt92xxDevice Read");
    uint8_t touch_stat = 0;
    uint8_t retry_cnt = 0;
    // Datasheet implies that it is not guaranteed that report will be
    // ready when interrupt is generated, so allow a couple retries to check
    // touch status.
    while (!(touch_stat & GT_REG_TOUCH_STATUS_READY) && (retry_cnt < 3)) {
      zx::result<uint8_t> read_status = Read(GT_REG_TOUCH_STATUS);
      if (read_status.is_ok()) {
        touch_stat = read_status.value();
      }
      if (!(touch_stat & GT_REG_TOUCH_STATUS_READY)) {
        retry_cnt++;
        zx_nanosleep(zx_deadline_after(ZX_MSEC(1)));
      }
    }

    if (touch_stat & GT_REG_TOUCH_STATUS_READY) {
      uint8_t num_reports = touch_stat & 0x0f;
      FingerReport reports[kMaxPoints];
      // Read touch reports
      zx_status_t status = Read(GT_REG_REPORTS, reinterpret_cast<uint8_t*>(&reports),
                                static_cast<uint8_t>(sizeof(FingerReport) * kMaxPoints));
      // Clear touch status after reading reports
      Write(GT_REG_TOUCH_STATUS, 0);
      if (status == ZX_OK) {
        fbl::AutoLock lock(&client_lock_);
        gt_rpt_.rpt_id = GT92XX_RPT_ID_TOUCH;
        gt_rpt_.contact_count = num_reports;
        // We are reusing same HID report as ft3x77 to simplify astro integration
        // so we need to copy from device format to HID structure format
        for (uint32_t i = 0; i < kMaxPoints; i++) {
          gt_rpt_.fingers[i].finger_id =
              static_cast<uint8_t>((reports[i].id << 2) | ((i < num_reports) ? 1 : 0));
          gt_rpt_.fingers[i].y = reports[i].x;
          gt_rpt_.fingers[i].x = reports[i].y;
        }
        if (client_.is_valid()) {
          client_.IoQueue(reinterpret_cast<uint8_t*>(&gt_rpt_), sizeof(gt92xx_touch_t),
                          timestamp.get());
        }
      }
    } else {
      zxlogf(ERROR, "gt92xx: Errant interrupt, no report ready - %x", touch_stat);
    }
  }
  zxlogf(INFO, "gt92xx: exiting");
  return 0;
}

zx_status_t Gt92xxDevice::Create(zx_device_t* device) {
  zxlogf(INFO, "gt92xx: driver started...");
  ddk::I2cChannel i2c(device, "i2c");
  if (!i2c.is_valid()) {
    zxlogf(ERROR, "failed to acquire i2c");
    return ZX_ERR_NO_RESOURCES;
  }

  const char* kInterruptGpioFragmentName = "gpio-int";
  zx::result int_gpio = DdkConnectFragmentFidlProtocol<fuchsia_hardware_gpio::Service::Device>(
      device, kInterruptGpioFragmentName);
  if (int_gpio.is_error()) {
    zxlogf(ERROR, "Failed to get gpio protocol from fragment %s: %s", kInterruptGpioFragmentName,
           int_gpio.status_string());
    return ZX_ERR_NO_RESOURCES;
  }

  const char* kResetGpioFragmentName = "gpio-reset";
  zx::result reset_gpio = DdkConnectFragmentFidlProtocol<fuchsia_hardware_gpio::Service::Device>(
      device, kResetGpioFragmentName);
  if (reset_gpio.is_error()) {
    zxlogf(ERROR, "Failed to get gpio protocol from fragment %s: %s", kResetGpioFragmentName,
           reset_gpio.status_string());
    return ZX_ERR_NO_RESOURCES;
  }

  auto goodix_dev = std::make_unique<Gt92xxDevice>(
      device, std::move(i2c), std::move(int_gpio.value()), std::move(reset_gpio.value()));

  zx_status_t status = goodix_dev->Init();
  if (status != ZX_OK) {
    zxlogf(ERROR, "Could not initialize gt92xx hardware %d", status);
    return status;
  }

  auto thunk = [](void* arg) -> int { return reinterpret_cast<Gt92xxDevice*>(arg)->Thread(); };

  auto cleanup = fit::defer([&]() { goodix_dev->ShutDown(); });

  goodix_dev->running_.store(true);
  int ret = thrd_create_with_name(&goodix_dev->thread_, thunk, goodix_dev.get(), "gt92xx-thread");
  ZX_DEBUG_ASSERT(ret == thrd_success);

  // Set scheduler role for device thread.
  {
    const char* role_name = "fuchsia.ui.input.drivers.goodix.gt92xx.device";
    status =
        device_set_profile_by_role(goodix_dev->parent(), thrd_get_zx_handle(goodix_dev->thread_),
                                   role_name, strlen(role_name));
    if (status != ZX_OK) {
      zxlogf(WARNING, "Gt92xxDevice::Create: Failed to apply role: %s",
             zx_status_get_string(status));
    }
  }

  status = goodix_dev->DdkAdd(ddk::DeviceAddArgs("gt92xx HidDevice")
                                  .set_inspect_vmo(goodix_dev->inspector_.DuplicateVmo()));
  if (status != ZX_OK) {
    zxlogf(ERROR, "gt92xx: Could not create hid device: %d", status);
    return status;
  } else {
    zxlogf(INFO, "gt92xx: Added hid device");
  }

  cleanup.cancel();

  // device intentionally leaked as it is now held by DevMgr
  [[maybe_unused]] auto ptr = goodix_dev.release();

  return ZX_OK;
}

zx_status_t Gt92xxDevice::Init() {
  // Hardware reset
  HWReset();

  zx_status_t status = UpdateFirmwareIfNeeded();
  if (status != ZX_OK) {
    return status;
  }

  // Get the config data
  fbl::Vector<uint8_t> Conf(GetConfData());

  // Configuration data should span specific set of registers
  // last register has flag to latch in new configuration, second
  // to last register holds checksum of register values.
  // Note: first two bytes of conf_data hold the 16-bit register address where
  // the write will start.
  ZX_DEBUG_ASSERT((Conf.size() - sizeof(uint16_t)) ==
                  (GT_REG_CONFIG_REFRESH - GT_REG_CONFIG_DATA + 1));

  zx::result<uint8_t> version = Read(GT_REG_CONFIG_DATA);
  if (version.is_ok() && version.value() > Conf.data()[sizeof(uint16_t)]) {
    // Force the controller to take the config.
    Conf[sizeof(uint16_t)] = 0x00;
  }

  // Write conf data to registers
  status = Write(Conf.data(), Conf.size());
  if (status != ZX_OK) {
    return status;
  }
  // Device requires 10ms delay to refresh configuration
  zx_nanosleep(zx_deadline_after(ZX_MSEC(10)));
  // Clear touch state in case there were spurious touches registered
  // during startup
  Write(GT_REG_TOUCH_STATUS, 0);

  // Note: Our configuration inverts polarity of interrupt
  // (datasheet implies it is active high)
  fidl::WireResult interrupt_result = int_gpio_->GetInterrupt(ZX_INTERRUPT_MODE_EDGE_LOW);
  if (!interrupt_result.ok()) {
    zxlogf(ERROR, "Failed to send GetInterrupt request to interrupt gpio: %s",
           interrupt_result.status_string());
    return interrupt_result.status();
  }
  if (interrupt_result->is_error()) {
    zxlogf(ERROR, "Failed to get interrupt from interrupt gpio: %s",
           zx_status_get_string(interrupt_result->error_value()));
    return interrupt_result->error_value();
  }
  irq_ = std::move(interrupt_result.value()->irq);

  LogFirmwareStatus();
  return ZX_OK;
}

zx_status_t Gt92xxDevice::HWReset() {
  // Hardware reset will also set the address of the controller to either
  // 0x14 0r 0x5d.  See the datasheet for explanation of sequence.
  {
    fidl::WireResult result = reset_gpio_->ConfigOut(0);  // Make reset pin an output and pull low
    if (!result.ok()) {
      zxlogf(ERROR, "Failed to send ConfigOut request to reset gpio: %s", result.status_string());
      return result.status();
    }
    if (result->is_error()) {
      zxlogf(ERROR, "Failed to configure reset gpio to output: %s",
             zx_status_get_string(result->error_value()));
      return result->error_value();
    }
  }

  {
    fidl::WireResult result = int_gpio_->ConfigOut(0);  // Make interrupt pin an output and pull low
    if (!result.ok()) {
      zxlogf(ERROR, "Failed to send ConfigOut request to interrupt gpio: %s",
             result.status_string());
      return result.status();
    }
    if (result->is_error()) {
      zxlogf(ERROR, "Failed to configure interrupt gpio to output: %s",
             zx_status_get_string(result->error_value()));
      return result->error_value();
    }
  }

  // Delay for 100us
  zx_nanosleep(zx_deadline_after(ZX_USEC(100)));
  {
    fidl::WireResult result = reset_gpio_->Write(1);  // Release the reset
    if (!result.ok()) {
      zxlogf(ERROR, "Failed to send Write request to reset gpio: %s", result.status_string());
      return result.status();
    }
    if (result->is_error()) {
      zxlogf(ERROR, "Failed to write to reset gpio: %s",
             zx_status_get_string(result->error_value()));
      return result->error_value();
    }
  }
  zx_nanosleep(zx_deadline_after(ZX_MSEC(5 + 50)));  // Interrupt still output low
  {
    fidl::WireResult result = int_gpio_->ConfigIn(
        fuchsia_hardware_gpio::GpioFlags::kPullUp);  // Make interrupt pin an input again
    if (!result.ok()) {
      zxlogf(ERROR, "Failed to send ConfigIn request to interrupt gpio: %s",
             result.status_string());
      return result.status();
    }
    if (result->is_error()) {
      zxlogf(ERROR, "Failed to configure interrupt gpio to input: %s",
             zx_status_get_string(result->error_value()));
      return result->error_value();
    }
  }
  zx_nanosleep(zx_deadline_after(ZX_MSEC(50)));  // Wait for reset before sending config
  return ZX_OK;
}

zx_status_t Gt92xxDevice::HidbusQuery(uint32_t options, hid_info_t* info) {
  if (!info) {
    return ZX_ERR_INVALID_ARGS;
  }
  info->dev_num = 0;
  info->device_class = HID_DEVICE_CLASS_OTHER;
  info->boot_device = false;

  return ZX_OK;
}

void Gt92xxDevice::DdkRelease() { delete this; }

void Gt92xxDevice::DdkUnbind(ddk::UnbindTxn txn) {
  ShutDown();
  txn.Reply();
}

zx_status_t Gt92xxDevice::ShutDown() {
  running_.store(false);
  irq_.destroy();
  thrd_join(thread_, NULL);
  {
    fbl::AutoLock lock(&client_lock_);
    client_.clear();
  }
  return ZX_OK;
}

zx_status_t Gt92xxDevice::HidbusGetDescriptor(hid_description_type_t desc_type,
                                              uint8_t* out_data_buffer, size_t data_size,
                                              size_t* out_data_actual) {
  const uint8_t* desc;
  size_t desc_size = get_gt92xx_report_desc(&desc);
  if (data_size < desc_size) {
    return ZX_ERR_BUFFER_TOO_SMALL;
  }
  memcpy(out_data_buffer, desc, desc_size);
  *out_data_actual = desc_size;

  return ZX_OK;
}

zx_status_t Gt92xxDevice::HidbusGetReport(uint8_t rpt_type, uint8_t rpt_id, uint8_t* data,
                                          size_t len, size_t* out_len) {
  return ZX_ERR_NOT_SUPPORTED;
}

zx_status_t Gt92xxDevice::HidbusSetReport(uint8_t rpt_type, uint8_t rpt_id, const uint8_t* data,
                                          size_t len) {
  return ZX_ERR_NOT_SUPPORTED;
}

zx_status_t Gt92xxDevice::HidbusGetIdle(uint8_t rpt_id, uint8_t* duration) {
  return ZX_ERR_NOT_SUPPORTED;
}

zx_status_t Gt92xxDevice::HidbusSetIdle(uint8_t rpt_id, uint8_t duration) {
  return ZX_ERR_NOT_SUPPORTED;
}

zx_status_t Gt92xxDevice::HidbusGetProtocol(uint8_t* protocol) { return ZX_ERR_NOT_SUPPORTED; }

zx_status_t Gt92xxDevice::HidbusSetProtocol(uint8_t protocol) { return ZX_OK; }

void Gt92xxDevice::HidbusStop() {
  fbl::AutoLock lock(&client_lock_);
  client_.clear();
}

zx_status_t Gt92xxDevice::HidbusStart(const hidbus_ifc_protocol_t* ifc) {
  fbl::AutoLock lock(&client_lock_);
  if (client_.is_valid()) {
    zxlogf(ERROR, "gt92xx: Already bound!");
    return ZX_ERR_ALREADY_BOUND;
  } else {
    client_ = ddk::HidbusIfcProtocolClient(ifc);
    zxlogf(INFO, "gt92xx: started");
  }
  return ZX_OK;
}

zx::result<uint8_t> Gt92xxDevice::Read(uint16_t addr) {
  uint8_t rbuf;
  zx_status_t status = Read(addr, &rbuf, 1);
  if (status != ZX_OK) {
    return zx::error(status);
  }
  return zx::ok(rbuf);
}

zx_status_t Gt92xxDevice::Read(uint16_t addr, uint8_t* buf, size_t len) {
  zx_status_t status;
  for (int i = 0; i < kI2cRetries; i++) {
    uint8_t tbuf[2];
    tbuf[0] = static_cast<uint8_t>(addr >> 8);
    tbuf[1] = static_cast<uint8_t>(addr & 0xff);
    if ((status = i2c_.WriteReadSync(tbuf, 2, buf, len)) == ZX_OK) {
      return ZX_OK;
    }
  }
  zxlogf(ERROR, "Failed to read %s 0x%04x: %d", len <= 1 ? "register" : "from", addr, status);
  return status;
}

zx_status_t Gt92xxDevice::Write(uint16_t addr, uint8_t val) {
  zx_status_t status;
  for (int i = 0; i < kI2cRetries; i++) {
    uint8_t tbuf[3];
    tbuf[0] = static_cast<uint8_t>(addr >> 8);
    tbuf[1] = static_cast<uint8_t>(addr & 0xff);
    tbuf[2] = val;
    if ((status = i2c_.WriteSync(tbuf, 3)) == ZX_OK) {
      return ZX_OK;
    }
  }
  zxlogf(ERROR, "Failed to write register 0x%04x: %d", addr, status);
  return status;
}

zx_status_t Gt92xxDevice::Write(uint8_t* buf, size_t len) {
  zx_status_t status;
  for (int i = 0; i < kI2cRetries; i++) {
    if ((status = i2c_.WriteSync(buf, len)) == ZX_OK) {
      return ZX_OK;
    }
  }
  zxlogf(ERROR, "Failed to write to 0x%04x: %d", (buf[0] << 8) | buf[1], status);
  return status;
}

}  // namespace goodix

zx_status_t gt92xx_bind(void* ctx, zx_device_t* device) {
  return goodix::Gt92xxDevice::Create(device);
}

static constexpr zx_driver_ops_t gt92xx_driver_ops = []() {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = gt92xx_bind;
  return ops;
}();

ZIRCON_DRIVER(gt92xx, gt92xx_driver_ops, "zircon", "0.1");
