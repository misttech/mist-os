// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "aml-i2c.h"

#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/trace/event.h>
#include <lib/zx/profile.h>
#include <lib/zx/thread.h>
#include <zircon/status.h>
#include <zircon/threads.h>
#include <zircon/types.h>

#include <memory>

#include <soc/aml-common/aml-i2c.h>

#include "aml-i2c-regs.h"

namespace {

constexpr zx_signals_t kErrorSignal = ZX_USER_SIGNAL_0;
constexpr zx_signals_t kTxnCompleteSignal = ZX_USER_SIGNAL_1;

constexpr size_t kMaxTransferSize = 512;

}  // namespace

namespace aml_i2c {

void AmlI2c::SetTargetAddr(uint16_t addr) const {
  addr &= 0x7f;
  TargetAddr::Get().ReadFrom(&regs_iobuff_).set_target_address(addr).WriteTo(&regs_iobuff_);
}

int AmlI2c::IrqThread() const {
  while (true) {
    zx_status_t status = irq_.wait(nullptr);
    if (status == ZX_ERR_CANCELED) {
      break;
    }
    if (status != ZX_OK) {
      zxlogf(DEBUG, "interrupt error: %s", zx_status_get_string(status));
      continue;
    }
    if (Control::Get().ReadFrom(&regs_iobuff_).error()) {
      event_.signal(0, kErrorSignal);
    } else {
      event_.signal(0, kTxnCompleteSignal);
    }
  }
  return ZX_OK;
}

#if 0
zx_status_t AmlI2c::DumpState() {
  printf("control reg      : %08x\n", regs_iobuff_.Read32(kControlReg));
  printf("target addr reg  : %08x\n", regs_iobuff_.Read32(kTargetAddrReg));
  printf("token list0 reg  : %08x\n", regs_iobuff_.Read32(kTokenList0Reg));
  printf("token list1 reg  : %08x\n", regs_iobuff_.Read32(kTokenList1Reg));
  printf("token wdata0     : %08x\n", regs_iobuff_.Read32(kWriteData0Reg));
  printf("token wdata1     : %08x\n", regs_iobuff_.Read32(kWriteData1Reg));
  printf("token rdata0     : %08x\n", regs_iobuff_.Read32(kReadData0Reg));
  printf("token rdata1     : %08x\n", regs_iobuff_.Read32(kReadData1Reg));

  return ZX_OK;
}
#endif

void AmlI2c::StartXfer() const {
  // First have to clear the start bit before setting (RTFM)
  Control::Get()
      .ReadFrom(&regs_iobuff_)
      .set_start(0)
      .WriteTo(&regs_iobuff_)
      .set_start(1)
      .WriteTo(&regs_iobuff_);
}

zx_status_t AmlI2c::WaitTransferComplete() const {
  constexpr zx_signals_t kSignalMask = kTxnCompleteSignal | kErrorSignal;

  uint32_t observed;
  zx_status_t status = event_.wait_one(kSignalMask, zx::deadline_after(timeout_), &observed);
  if (status != ZX_OK) {
    return status;
  }
  event_.signal(observed, 0);
  if (observed & kErrorSignal) {
    return ZX_ERR_TIMED_OUT;
  }
  return ZX_OK;
}

zx_status_t AmlI2c::Write(const uint8_t* buff, uint32_t len, const bool stop) const {
  TRACE_DURATION("i2c", "aml-i2c Write");
  ZX_DEBUG_ASSERT(len <= kMaxTransferSize);

  TokenList tokens = TokenList::Get().FromValue(0);
  tokens.Push(TokenList::Token::kStart);
  tokens.Push(TokenList::Token::kTargetAddrWr);

  while (len > 0) {
    const bool is_last_iter = len <= WriteData::kMaxWriteBytesPerTransfer;
    const uint32_t tx_size = is_last_iter ? len : WriteData::kMaxWriteBytesPerTransfer;
    for (uint32_t i = 0; i < tx_size; i++) {
      tokens.Push(TokenList::Token::kData);
    }

    if (is_last_iter && stop) {
      tokens.Push(TokenList::Token::kStop);
    }

    tokens.WriteTo(&regs_iobuff_);

    WriteData wdata = WriteData::Get().FromValue(0);
    for (uint32_t i = 0; i < tx_size; i++) {
      wdata.Push(buff[i]);
    }

    wdata.WriteTo(&regs_iobuff_);

    StartXfer();
    // while (Control::Get().ReadFrom(&regs_iobuff_).status()) ;;    // wait for idle
    zx_status_t status = WaitTransferComplete();
    if (status != ZX_OK) {
      return status;
    }

    len -= tx_size;
    buff += tx_size;
  }

  return ZX_OK;
}

zx_status_t AmlI2c::Read(uint8_t* buff, uint32_t len, const bool stop) const {
  ZX_DEBUG_ASSERT(len <= kMaxTransferSize);
  TRACE_DURATION("i2c", "aml-i2c Read");

  TokenList tokens = TokenList::Get().FromValue(0);
  tokens.Push(TokenList::Token::kStart);
  tokens.Push(TokenList::Token::kTargetAddrRd);

  while (len > 0) {
    const bool is_last_iter = len <= ReadData::kMaxReadBytesPerTransfer;
    const uint32_t rx_size = is_last_iter ? len : ReadData::kMaxReadBytesPerTransfer;

    for (uint32_t i = 0; i < (rx_size - 1); i++) {
      tokens.Push(TokenList::Token::kData);
    }
    if (is_last_iter) {
      tokens.Push(TokenList::Token::kDataLast);
      if (stop) {
        tokens.Push(TokenList::Token::kStop);
      }
    } else {
      tokens.Push(TokenList::Token::kData);
    }

    tokens.WriteTo(&regs_iobuff_);

    // clear registers to prevent data leaking from last xfer
    ReadData rdata = ReadData::Get().FromValue(0).WriteTo(&regs_iobuff_);

    StartXfer();

    zx_status_t status = WaitTransferComplete();
    if (status != ZX_OK) {
      return status;
    }

    // while (Control::Get().ReadFrom(&regs_iobuff_).status()) ;;    // wait for idle

    rdata.ReadFrom(&regs_iobuff_);

    for (uint32_t i = 0; i < rx_size; i++) {
      buff[i] = rdata.Pop();
    }

    len -= rx_size;
    buff += rx_size;
  }

  return ZX_OK;
}

void AmlI2c::StartIrqThread() {
  thrd_create_with_name(
      &irqthrd_, [](void* ctx) { return reinterpret_cast<AmlI2c*>(ctx)->IrqThread(); }, this,
      "i2c_irq_thread");

  // Set role for IRQ thread.
  const char* role_name = "fuchsia.devices.i2c.drivers.aml-i2c.interrupt";
  const zx_status_t status = device_set_profile_by_role(zxdev(), thrd_get_zx_handle(irqthrd_),
                                                        role_name, strlen(role_name));
  if (status != ZX_OK) {
    zxlogf(WARNING, "Failed to apply role: %s", zx_status_get_string(status));
  }
}

zx_status_t AmlI2c::SetClockDelay(zx_device_t* parent, const fdf::MmioBuffer& regs_iobuff) {
  aml_i2c_delay_values delay{0, 0};
  size_t actual;
  zx_status_t status =
      device_get_metadata(parent, DEVICE_METADATA_PRIVATE, &delay, sizeof(delay), &actual);
  if (status != ZX_OK) {
    if (status != ZX_ERR_NOT_FOUND) {
      zxlogf(ERROR, "device_get_metadata failed: %s", zx_status_get_string(status));
      return status;
    }
  } else if (actual != sizeof(delay)) {
    zxlogf(ERROR, "metadata size mismatch");
    return ZX_ERR_INTERNAL;
  }

  if (delay.quarter_clock_delay > Control::kQtrClkDlyMax ||
      delay.clock_low_delay > TargetAddr::kSclLowDelayMax) {
    zxlogf(ERROR, "invalid clock delay");
    return ZX_ERR_INVALID_ARGS;
  }

  if (delay.quarter_clock_delay > 0) {
    Control::Get()
        .ReadFrom(&regs_iobuff)
        .set_qtr_clk_dly(delay.quarter_clock_delay)
        .WriteTo(&regs_iobuff);
  }

  if (delay.clock_low_delay > 0) {
    TargetAddr::Get()
        .FromValue(0)
        .set_scl_low_dly(delay.clock_low_delay)
        .set_use_cnt_scl_low(1)
        .WriteTo(&regs_iobuff);
  }

  return ZX_OK;
}

zx_status_t AmlI2c::I2cImplGetMaxTransferSize(size_t* out_size) {
  *out_size = kMaxTransferSize;
  return ZX_OK;
}

zx_status_t AmlI2c::I2cImplSetBitrate(uint32_t bitrate) {
  // TODO(hollande,voydanoff) implement this
  return ZX_ERR_NOT_SUPPORTED;
}

zx_status_t AmlI2c::I2cImplTransact(const i2c_impl_op_t* rws, size_t count) {
  TRACE_DURATION("i2c", "aml-i2c Transact");
  for (size_t i = 0; i < count; ++i) {
    if (rws[i].data_size > kMaxTransferSize) {
      return ZX_ERR_OUT_OF_RANGE;
    }
  }

  for (size_t i = 0; i < count; ++i) {
    SetTargetAddr(rws[i].address);

    zx_status_t status;
    if (rws[i].is_read) {
      status = Read(rws[i].data_buffer, static_cast<uint32_t>(rws[i].data_size), rws[i].stop);
    } else {
      status = Write(rws[i].data_buffer, static_cast<uint32_t>(rws[i].data_size), rws[i].stop);
    }
    if (status != ZX_OK) {
      return status;  // TODO(andresoportus) release the bus
    }
  }

  return ZX_OK;
}

zx_status_t AmlI2c::Bind(void* ctx, zx_device_t* parent) {
  ddk::PDevFidl pdev(parent);
  if (!pdev.is_valid()) {
    pdev = ddk::PDevFidl(parent, "pdev");
  }
  if (!pdev.is_valid()) {
    zxlogf(ERROR, "ZX_PROTOCOL_PDEV not available");
    return ZX_ERR_NO_RESOURCES;
  }

  pdev_device_info_t info;
  zx_status_t status = pdev.GetDeviceInfo(&info);
  if (status != ZX_OK) {
    zxlogf(ERROR, "pdev_get_device_info failed: %s", zx_status_get_string(status));
    return status;
  }

  if (info.mmio_count != 1 || info.irq_count != 1) {
    zxlogf(ERROR, "Invalid mmio_count (%u) or irq_count (%u)", info.mmio_count, info.irq_count);
    return ZX_ERR_INVALID_ARGS;
  }

  std::optional<fdf::MmioBuffer> regs_iobuff;
  status = pdev.MapMmio(0, &regs_iobuff);
  if (status != ZX_OK) {
    zxlogf(ERROR, "pdev_map_mmio_buffer failed: %s", zx_status_get_string(status));
    return status;
  }

  status = SetClockDelay(parent, *regs_iobuff);
  if (status != ZX_OK) {
    return status;
  }

  zx::interrupt irq;
  status = pdev.GetInterrupt(0, 0, &irq);
  if (status != ZX_OK) {
    zxlogf(ERROR, "pdev_get_interrupt failed: %s", zx_status_get_string(status));
    return status;
  }

  zx::event event;
  status = zx::event::create(0, &event);
  if (status != ZX_OK) {
    zxlogf(ERROR, "zx_event_create failed: %s", zx_status_get_string(status));
    return status;
  }

  auto i2c =
      std::make_unique<AmlI2c>(parent, std::move(irq), std::move(event), *std::move(regs_iobuff));

  status = i2c->DdkAdd(
      ddk::DeviceAddArgs("aml-i2c").forward_metadata(parent, DEVICE_METADATA_I2C_CHANNELS));
  if (status != ZX_OK) {
    zxlogf(ERROR, "device_add failed");
    return status;
  }

  i2c->StartIrqThread();

  [[maybe_unused]] auto* unused = i2c.release();
  return status;
}

}  // namespace aml_i2c

static zx_driver_ops_t aml_i2c_driver_ops = {
    .version = DRIVER_OPS_VERSION,
    .bind = aml_i2c::AmlI2c::Bind,
};

ZIRCON_DRIVER(aml_i2c, aml_i2c_driver_ops, "zircon", "0.1");
