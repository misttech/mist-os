// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "imx8m-i2c.h"

#include <fuchsia/hardware/i2cimpl/c/banjo.h>
#include <fuchsia/hardware/platform/device/c/banjo.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <lib/fit/defer.h>
#include <lib/mmio/mmio-buffer.h>
#include <lib/sync/completion.h>
#include <lib/zx/clock.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <threads.h>
#include <unistd.h>
#include <zircon/assert.h>
#include <zircon/process.h>

#include <memory>

#include <fbl/alloc_checker.h>
#include <fbl/auto_lock.h>

namespace imx8m_i2c {

static constexpr uint32_t kStandardModeFrequencyHz = 100'000;

static constexpr size_t kMaxTransferSize = (UINT16_MAX - 1);
// TODO(fxbug.dev/121200): Replace kI2cClockHz with ClockGetRate()
static constexpr size_t kI2cClockHz = 24'000'000;

// RM: Table 16-1. I2C_IFDR Register Field Values
static const uint32_t divider_table[] = {
    30,  32,  36,  42,  48,  52,  60,  72,  80,   88,   104,  128,  144,  160,  192,  240,
    288, 320, 384, 480, 576, 640, 768, 960, 1152, 1280, 1536, 1920, 2304, 2560, 3072, 3840,
    22,  24,  26,  28,  32,  36,  40,  44,  48,   56,   64,   72,   80,   96,   112,  128,
    160, 192, 224, 256, 320, 384, 448, 512, 640,  768,  896,  1024, 1280, 1536, 1792, 2048};

void Imx8mI2c::HandleIrq(async_dispatcher_t* dispatcher, async::IrqBase* irq, zx_status_t status,
                         const zx_packet_interrupt_t* interrupt) {
  if (status != ZX_OK) {
    return;
  }

  auto status_reg = StatusReg::Get().ReadFrom(&mmio_);

  if (status_reg.ial()) {
    if (event_.signal(0, kTransactionError) != ZX_OK) {
      zxlogf(ERROR, "Error %s while signaling error", zx_status_get_string(status));
    }
  } else if (status_reg.iif()) {
    if (event_.signal(0, kTransactionCompleteSignal) != ZX_OK) {
      zxlogf(ERROR, "Error %s while signaling complete", zx_status_get_string(status));
    }
  }

  status_reg.set_ial(0).set_iif(0).WriteTo(&mmio_);

  irq_.ack();
}

zx_status_t Imx8mI2c::WaitForBusState(bool state) {
  const zx::time deadline = zx::clock::get_monotonic() + zx::msec(500);

  do {
    if (StatusReg::Get().ReadFrom(&mmio_).ibb() == static_cast<uint8_t>(state)) {
      return ZX_OK;
    }
    zx::nanosleep(zx::deadline_after(zx::usec(100)));
  } while (zx::clock::get_monotonic() <= deadline);

  return ZX_ERR_TIMED_OUT;
}

zx_status_t Imx8mI2c::PreStart() {
  auto status_reg = StatusReg::Get().ReadFrom(&mmio_);
  if (status_reg.ial()) {
    zxlogf(ERROR, "Arbitration lost");
    status_reg.set_ial(0).WriteTo(&mmio_);
    return ZX_ERR_BAD_STATE;
  }

  // Wait until bus becomes idle
  zx_status_t status = WaitForBusState(0);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Timed out waiting for bus to be idle");
    return status;
  }

  status_reg.set_ial(0).set_iif(0).WriteTo(&mmio_);

  return ZX_OK;
}

zx_status_t Imx8mI2c::Start() {
  ControlReg::Get().ReadFrom(&mmio_).set_msta(1).set_mtx(1).set_txak(1).set_iien(1).WriteTo(&mmio_);

  // Wait until bus becomes busy
  zx_status_t status = WaitForBusState(1);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Timed out waiting for bus to be busy");
    return status;
  }

  return ZX_OK;
}

zx_status_t Imx8mI2c::RepeatedStart() {
  auto control_reg = ControlReg::Get().ReadFrom(&mmio_);
  control_reg.set_rsta(1).WriteTo(&mmio_);

  // Wait until bus becomes busy
  zx_status_t status = WaitForBusState(1);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Timed out waiting for bus to be busy");
    return status;
  }

  return ZX_OK;
}

zx_status_t Imx8mI2c::Stop() {
  auto control_reg = ControlReg::Get().ReadFrom(&mmio_);
  control_reg.set_msta(0).set_mtx(0).set_iien(0).WriteTo(&mmio_);

  // Wait until bus becomes idle
  zx_status_t status = WaitForBusState(0);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Timed out waiting for bus to be idle");
    return status;
  }

  return ZX_OK;
}

zx_status_t Imx8mI2c::IsAcked() {
  auto status_reg = StatusReg::Get().ReadFrom(&mmio_);

  if (status_reg.rxak()) {
    return ZX_ERR_IO;
  }

  return ZX_OK;
}

zx_status_t Imx8mI2c::WaitForTransactionComplete() {
  uint32_t observed = 0;
  uint32_t sig_mask = kTransactionCompleteSignal | kTransactionError;
  auto deadline = zx::deadline_after(timeout_);

  zx_status_t status = event_.wait_one(sig_mask, deadline, &observed);
  if (status != ZX_OK) {
    DumpRegs();
    return status;
  }

  event_.signal(observed, 0);
  if (!(observed & kTransactionCompleteSignal)) {
    DumpRegs();
    return ZX_ERR_BAD_STATE;
  }

  return ZX_OK;
}

zx_status_t Imx8mI2c::SendAddress(const i2c_impl_op_t& op) {
  uint16_t addr = static_cast<uint16_t>(op.address << 1 | static_cast<uint16_t>(op.is_read));

  DataReg::Get().FromValue(0).set_data(addr).WriteTo(&mmio_);
  zx_status_t status = WaitForTransactionComplete();
  if (status != ZX_OK) {
    return status;
  }

  return IsAcked();
}

zx_status_t Imx8mI2c::Transmit(const i2c_impl_op_t& op) {
  zx_status_t status = SendAddress(op);
  if (status != ZX_OK) {
    zxlogf(DEBUG, "Addr send failed with error %s", zx_status_get_string(status));
    return status;
  }

  for (uint32_t i = 0; i < op.data_size; i++) {
    DataReg::Get().FromValue(0).set_data(op.data_buffer[i]).WriteTo(&mmio_);

    zx_status_t status = WaitForTransactionComplete();
    if (status != ZX_OK) {
      zxlogf(ERROR, "Error %s while waiting for transaction complete",
             zx_status_get_string(status));
      return status;
    }

    status = IsAcked();
    if (status != ZX_OK) {
      zxlogf(ERROR, "Data transaction not acked");
      return status;
    }
  }

  return ZX_OK;
}

zx_status_t Imx8mI2c::Receive(const i2c_impl_op_t& op, bool last_msg) {
  zx_status_t status = SendAddress(op);
  if (status != ZX_OK) {
    zxlogf(DEBUG, "Addr send failed with error %s", zx_status_get_string(status));
    return status;
  }

  auto control_reg = ControlReg::Get().ReadFrom(&mmio_);
  control_reg.set_mtx(0);
  if (op.data_size > 1) {
    control_reg.set_txak(0);
  }
  control_reg.WriteTo(&mmio_);

  // extra read to trigger receive data (as per RM)
  DataReg::Get().ReadFrom(&mmio_);

  for (uint32_t i = 0; i < op.data_size; i++) {
    status = WaitForTransactionComplete();
    if (status != ZX_OK) {
      zxlogf(ERROR, "Error %s while waiting for transaction complete",
             zx_status_get_string(status));
      return status;
    }

    if (i == (op.data_size - 1)) {
      if (last_msg) {
        // generate STOP before reading I2DR to prevent
        // controller from generating another clock cycle.
        status = Stop();
        if (status != ZX_OK) {
          return status;
        }
      } else {
        control_reg.set_mtx(1).WriteTo(&mmio_);
      }
    } else if (i == (op.data_size - 2)) {
      control_reg.set_txak(1).WriteTo(&mmio_);
    }

    op.data_buffer[i] = static_cast<uint8_t>(DataReg::Get().ReadFrom(&mmio_).data());
  }

  return ZX_OK;
}

zx_status_t Imx8mI2c::Transact(const i2c_impl_op_t* op_list, size_t count) {
  fbl::AutoLock lock(&transact_lock_);

  zx_status_t status = PreStart();
  if (status != ZX_OK) {
    return status;
  }

  status = Start();
  if (status != ZX_OK) {
    return status;
  }

  for (uint32_t i = 0; i < count; i++) {
    if (i) {
      if (op_list[i].stop) {
        status = Stop();
        if (status != ZX_OK) {
          return status;
        }
        status = Start();
        if (status != ZX_OK) {
          return status;
        }
      } else {
        status = RepeatedStart();
        if (status != ZX_OK) {
          return status;
        }
      }
    }

    if (op_list[i].is_read) {
      status = Receive(op_list[i], i == (count - 1));
      if (status != ZX_OK) {
        return status;
      }
    } else {
      status = Transmit(op_list[i]);
      if (status != ZX_OK) {
        return status;
      }
    }
  }

  // If the last transactions is read then
  // stop is already done in Receive().
  if (!op_list[count - 1].is_read) {
    status = Stop();
  }

  return status;
}

void Imx8mI2c::Shutdown() { irq_handler_.Cancel(); }

void Imx8mI2c::DumpRegs() {
  zxlogf(INFO, "IADR = 0x%x", AddressReg::Get().ReadFrom(&mmio_).reg_value());
  zxlogf(INFO, "IFDR = 0x%x", FrequencyDividerRegister::Get().ReadFrom(&mmio_).reg_value());
  zxlogf(INFO, "I2CR = 0x%x", ControlReg::Get().ReadFrom(&mmio_).reg_value());
  zxlogf(INFO, "I2SR = 0x%x", StatusReg::Get().ReadFrom(&mmio_).reg_value());
  zxlogf(INFO, "I2DR = 0x%x", DataReg::Get().ReadFrom(&mmio_).reg_value());
}

zx_status_t Imx8mI2c::HostInit() {
  ControlReg::Get().ReadFrom(&mmio_).set_ien(0).WriteTo(&mmio_);

  StatusReg::Get().ReadFrom(&mmio_).set_ial(0).set_iif(0).WriteTo(&mmio_);

  SetBitRate(kStandardModeFrequencyHz);

  ControlReg::Get().ReadFrom(&mmio_).set_ien(1).WriteTo(&mmio_);

  zx_nanosleep(zx_deadline_after(ZX_USEC(100)));

  return ZX_OK;
}

zx_status_t Imx8mI2c::SetBitRate(uint32_t bitrate) {
  uint32_t computed_rate;
  uint32_t abs_error;
  uint32_t best_error = UINT32_MAX;
  uint16_t best_ic = 0;

  // Scan table to find best match
  for (uint16_t i = 0; i < std::size(divider_table); ++i) {
    computed_rate = kI2cClockHz / divider_table[i];
    abs_error = bitrate > computed_rate ? (bitrate - computed_rate) : (computed_rate - bitrate);

    if (abs_error < best_error) {
      best_ic = i;
      best_error = abs_error;

      // If the error is 0, then we can stop searching because we won't find a better match
      if (abs_error == 0U) {
        break;
      }
    }
  }

  // Set frequency register based on best settings
  FrequencyDividerRegister::Get().ReadFrom(&mmio_).set_ic(best_ic).WriteTo(&mmio_);

  return ZX_OK;
}

void Imx8mI2c::SetTimeout(zx::duration timeout) { timeout_ = timeout; }

zx_status_t Imx8mI2c::Init() {
  timeout_ = zx::duration(kDefaultTimeout);

  zx_status_t status = zx::event::create(0, &event_);
  if (status != ZX_OK) {
    return status;
  }

  irq_handler_.set_object(irq_.get());
  irq_handler_.Begin(fdf::Dispatcher::GetCurrent()->async_dispatcher());

  return HostInit();
}

zx_status_t Imx8mI2c::I2cImplTransact(const i2c_impl_op_t* op_list, size_t count) {
  for (uint32_t i = 0; i < count; ++i) {
    if (op_list[i].data_size > kMaxTransferSize) {
      return ZX_ERR_OUT_OF_RANGE;
    }
  }

  if (count == 0) {
    return ZX_OK;
  }

  for (uint32_t i = 1; i < count; ++i) {
    if (op_list[i].address != op_list[0].address) {
      return ZX_ERR_NOT_SUPPORTED;
    }
  }

  return Transact(op_list, count);
}

zx_status_t Imx8mI2c::I2cImplSetBitrate(uint32_t bitrate) { return SetBitRate(bitrate); }

zx_status_t Imx8mI2c::I2cImplGetMaxTransferSize(size_t* out_size) {
  *out_size = kMaxTransferSize;
  return ZX_OK;
}

void Imx8mI2c::DdkUnbind(ddk::UnbindTxn txn) {
  Shutdown();
  txn.Reply();
}

void Imx8mI2c::DdkRelease() { delete this; }

zx_status_t Imx8mI2c::Create(void* ctx, zx_device_t* parent) {
  zx_status_t status;
  fbl::AllocChecker ac;

  auto pdev = ddk::PDevFidl::FromFragment(parent);
  if (!pdev.is_valid()) {
    zxlogf(ERROR, "Could not get pdev.");
    return ZX_ERR_NO_RESOURCES;
  }

  pdev_device_info_t info;
  if ((status = pdev.GetDeviceInfo(&info)) != ZX_OK) {
    zxlogf(ERROR, "pdev_get_device_info failed: %s", zx_status_get_string(status));
    return status;
  }

  if (info.mmio_count != 1 || info.irq_count != 1) {
    zxlogf(ERROR, "Invalid mmio_count (%u) or irq_count (%u)", info.mmio_count, info.irq_count);
    return ZX_ERR_INVALID_ARGS;
  }

  // TODO(fxbug.dev/121200): Enable and Set clock using clock fragment
  std::optional<ddk::MmioBuffer> mmio;
  if ((status = pdev.MapMmio(0, &mmio)) != ZX_OK) {
    zxlogf(ERROR, "pdev_map_mmio_buffer failed %s", zx_status_get_string(status));
    return status;
  }

  zx::interrupt irq;
  if ((status = pdev.GetInterrupt(0, 0, &irq)) != ZX_OK) {
    zxlogf(ERROR, "pdev GetInterrupt failed: %s", zx_status_get_string(status));
    return status;
  }

  auto dev = fbl::make_unique_checked<Imx8mI2c>(&ac, parent, *std::move(mmio), irq);
  if (!ac.check()) {
    zxlogf(ERROR, "ZX_ERR_NO_MEMORY");
    return ZX_ERR_NO_MEMORY;
  }

  if ((status = dev->Init()) != ZX_OK) {
    zxlogf(ERROR, "imx8m_i2c bus init failed %s", zx_status_get_string(status));
    return ZX_ERR_INTERNAL;
  }

  status = dev->Bind();
  if (status != ZX_OK) {
    zxlogf(ERROR, "Bind failed %s", zx_status_get_string(status));
    return status;
  }

  // Devmgr is now in charge of the memory for dev.
  [[maybe_unused]] auto ptr = dev.release();
  return ZX_OK;
}

zx_status_t Imx8mI2c::Bind() {
  zx_status_t status = DdkAdd(ddk::DeviceAddArgs("imx8m-i2c")
                                  .set_proto_id(ddk_proto_id_)
                                  .forward_metadata(parent(), DEVICE_METADATA_I2C_CHANNELS));
  if (status != ZX_OK) {
    zxlogf(ERROR, "imx8m-i2c::Create: DdkAdd failed");
    return status;
  }

  return ZX_OK;
}

static zx_driver_ops_t imx8m_i2c_driver_ops = {
    .version = DRIVER_OPS_VERSION,
    .init = nullptr,
    .bind = Imx8mI2c::Create,
    .create = nullptr,
    .release = nullptr,
    .run_unit_tests = nullptr,
};

}  // namespace imx8m_i2c

// clang-format off
ZIRCON_DRIVER(imx8m_i2c, imx8m_i2c::imx8m_i2c_driver_ops, "zircon", "0.1");
//clang-format on
