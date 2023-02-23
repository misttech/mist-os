// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "aml-i2c.h"

#include <lib/ddk/debug.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/trace/event.h>
#include <lib/zx/profile.h>
#include <lib/zx/thread.h>
#include <zircon/status.h>
#include <zircon/threads.h>
#include <zircon/types.h>

#include <memory>

#include "aml-i2c-regs.h"
#include "src/devices/i2c/drivers/aml-i2c/aml_i2c_bind.h"

#define I2C_ERROR_SIGNAL ZX_USER_SIGNAL_0
#define I2C_TXN_COMPLETE_SIGNAL ZX_USER_SIGNAL_1

#define AML_I2C_MAX_TRANSFER 512

namespace aml_i2c {

zx_status_t AmlI2cDev::SetTargetAddr(uint16_t addr) const {
  addr &= 0x7f;
  TargetAddr::Get().ReadFrom(&(*regs_iobuff_)).set_target_address(addr).WriteTo(&(*regs_iobuff_));

  return ZX_OK;
}

int AmlI2cDev::IrqThread() const {
  while (true) {
    zx_status_t status = irq_.wait(nullptr);
    if (status == ZX_ERR_CANCELED) {
      break;
    }
    if (status != ZX_OK) {
      zxlogf(DEBUG, "interrupt error: %s", zx_status_get_string(status));
      continue;
    }
    if (Control::Get().ReadFrom(&(*regs_iobuff_)).error()) {
      event_.signal(0, I2C_ERROR_SIGNAL);
    } else {
      event_.signal(0, I2C_TXN_COMPLETE_SIGNAL);
    }
  }
  return ZX_OK;
}

#if 0
zx_status_t AmlI2cDev::DumpState() {
  printf("control reg      : %08x\n", regs_iobuff_->Read32(kControlReg));
  printf("target addr reg  : %08x\n", regs_iobuff_->Read32(kTargetAddrReg));
  printf("token list0 reg  : %08x\n", regs_iobuff_->Read32(kTokenList0Reg));
  printf("token list1 reg  : %08x\n", regs_iobuff_->Read32(kTokenList1Reg));
  printf("token wdata0     : %08x\n", regs_iobuff_->Read32(kWriteData0Reg));
  printf("token wdata1     : %08x\n", regs_iobuff_->Read32(kWriteData1Reg));
  printf("token rdata0     : %08x\n", regs_iobuff_->Read32(kReadData0Reg));
  printf("token rdata1     : %08x\n", regs_iobuff_->Read32(kReadData1Reg));

  return ZX_OK;
}
#endif

zx_status_t AmlI2cDev::StartXfer() const {
  // First have to clear the start bit before setting (RTFM)
  Control::Get()
      .ReadFrom(&(*regs_iobuff_))
      .set_start(0)
      .WriteTo(&(*regs_iobuff_))
      .set_start(1)
      .WriteTo(&(*regs_iobuff_));
  return ZX_OK;
}

zx_status_t AmlI2cDev::WaitEvent(uint32_t sig_mask) const {
  zx::time deadline = zx::deadline_after(timeout_);
  uint32_t observed;
  sig_mask |= I2C_ERROR_SIGNAL;
  zx_status_t status = event_.wait_one(sig_mask, deadline, &observed);
  if (status != ZX_OK) {
    return status;
  }
  event_.signal(observed, 0);
  if (observed & I2C_ERROR_SIGNAL)
    return ZX_ERR_TIMED_OUT;
  return ZX_OK;
}

zx_status_t AmlI2cDev::Write(const uint8_t* buff, uint32_t len, bool stop) const {
  TRACE_DURATION("i2c", "aml-i2c Write");
  ZX_DEBUG_ASSERT(len <= AML_I2C_MAX_TRANSFER);

  TokenList tokens = TokenList::Get().FromValue(0);
  tokens.Push(TokenList::Token::kStart);
  tokens.Push(TokenList::Token::kTargetAddrWr);

  while (len > 0) {
    bool is_last_iter = len <= WriteData::kMaxWriteBytesPerTransfer;
    uint32_t tx_size = is_last_iter ? len : WriteData::kMaxWriteBytesPerTransfer;
    for (uint32_t i = 0; i < tx_size; i++) {
      tokens.Push(TokenList::Token::kData);
    }

    if (is_last_iter && stop) {
      tokens.Push(TokenList::Token::kStop);
    }

    tokens.WriteTo(&(*regs_iobuff_));

    WriteData wdata = WriteData::Get().FromValue(0);
    for (uint32_t i = 0; i < tx_size; i++) {
      wdata.Push(buff[i]);
    }

    wdata.WriteTo(&(*regs_iobuff_));

    StartXfer();
    // while (Control::Get().ReadFrom(&(*regs_iobuff_)).status()) ;;    // wait for idle
    zx_status_t status = WaitEvent(I2C_TXN_COMPLETE_SIGNAL);
    if (status != ZX_OK) {
      return status;
    }

    len -= tx_size;
    buff += tx_size;
  }

  return ZX_OK;
}

zx_status_t AmlI2cDev::Read(uint8_t* buff, uint32_t len, bool stop) const {
  ZX_DEBUG_ASSERT(len <= AML_I2C_MAX_TRANSFER);
  TRACE_DURATION("i2c", "aml-i2c Read");

  TokenList tokens = TokenList::Get().FromValue(0);
  tokens.Push(TokenList::Token::kStart);
  tokens.Push(TokenList::Token::kTargetAddrRd);

  while (len > 0) {
    bool is_last_iter = len <= ReadData::kMaxReadBytesPerTransfer;
    uint32_t rx_size = is_last_iter ? len : ReadData::kMaxReadBytesPerTransfer;

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

    tokens.WriteTo(&(*regs_iobuff_));

    // clear registers to prevent data leaking from last xfer
    ReadData rdata = ReadData::Get().FromValue(0).WriteTo(&(*regs_iobuff_));

    StartXfer();

    zx_status_t status = WaitEvent(I2C_TXN_COMPLETE_SIGNAL);
    if (status != ZX_OK) {
      return status;
    }

    // while (Control::Get().ReadFrom(&(*regs_iobuff_)).status()) ;;    // wait for idle

    rdata.ReadFrom(&(*regs_iobuff_));

    for (uint32_t i = 0; i < rx_size; i++) {
      buff[i] = rdata.Pop();
    }

    len -= rx_size;
    buff += rx_size;
  }

  return ZX_OK;
}

/* create instance of AmlI2cDev and do basic initialization.  There will
be one of these instances for each of the soc i2c ports.
*/
zx_status_t AmlI2cDev::Init(unsigned index, aml_i2c_delay_values delay, ddk::PDev pdev) {
  zx_status_t status = pdev.MapMmio(index, &regs_iobuff_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "pdev_map_mmio_buffer failed: %s", zx_status_get_string(status));
    return status;
  }

  if (delay.quarter_clock_delay > Control::kQtrClkDlyMax ||
      delay.clock_low_delay > TargetAddr::kSclLowDelayMax) {
    zxlogf(ERROR, "invalid clock delay");
    return ZX_ERR_INVALID_ARGS;
  }

  if (delay.quarter_clock_delay > 0) {
    Control::Get()
        .ReadFrom(&(*regs_iobuff_))
        .set_qtr_clk_dly(delay.quarter_clock_delay)
        .WriteTo(&(*regs_iobuff_));
  }

  if (delay.clock_low_delay > 0) {
    TargetAddr::Get()
        .FromValue(0)
        .set_scl_low_dly(delay.clock_low_delay)
        .set_use_cnt_scl_low(1)
        .WriteTo(&(*regs_iobuff_));
  }

  status = pdev.GetInterrupt(index, 0, &irq_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "pdev_get_interrupt failed: %s", zx_status_get_string(status));
    return status;
  }

  status = zx::event::create(0, &event_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "zx_event_create failed: %s", zx_status_get_string(status));
    return status;
  }

  thrd_create_with_name(
      &irqthrd_, [](void* ctx) { return reinterpret_cast<AmlI2cDev*>(ctx)->IrqThread(); }, this,
      "i2c_irq_thread");

  // Set profile for IRQ thread.
  // TODO(fxbug.dev/40858): Migrate to the role-based API when available, instead of hard
  // coding parameters.
  const zx::duration capacity = zx::usec(20);
  const zx::duration deadline = zx::usec(100);
  const zx::duration period = deadline;

  zx::profile irq_profile;
  status = device_get_deadline_profile(nullptr, capacity.get(), deadline.get(), period.get(),
                                       "aml_i2c_irq_thread", irq_profile.reset_and_get_address());
  if (status != ZX_OK) {
    zxlogf(WARNING, "Failed to get deadline profile: %s", zx_status_get_string(status));
  } else {
    zx::unowned_thread thread(thrd_get_zx_handle(irqthrd_));
    status = thread->set_profile(irq_profile, 0);
    if (status != ZX_OK) {
      zxlogf(WARNING, "Failed to apply deadline profile to IRQ thread: %s",
             zx_status_get_string(status));
    }
  }

  return ZX_OK;
}

uint32_t AmlI2c::I2cImplGetBusCount() { return static_cast<uint32_t>(i2c_devs_.size()); }

uint32_t AmlI2c::I2cImplGetBusBase() { return 0; }

zx_status_t AmlI2c::I2cImplGetMaxTransferSize(uint32_t bus_id, size_t* out_size) {
  *out_size = AML_I2C_MAX_TRANSFER;
  return ZX_OK;
}

zx_status_t AmlI2c::I2cImplSetBitrate(uint32_t bus_id, uint32_t bitrate) {
  // TODO(hollande,voydanoff) implement this
  return ZX_ERR_NOT_SUPPORTED;
}

zx_status_t AmlI2c::I2cImplTransact(uint32_t bus_id, const i2c_impl_op_t* rws, size_t count) {
  TRACE_DURATION("i2c", "aml-i2c Transact");
  for (size_t i = 0; i < count; ++i) {
    if (rws[i].data_size > AML_I2C_MAX_TRANSFER) {
      return ZX_ERR_OUT_OF_RANGE;
    }
  }
  if (bus_id >= i2c_devs_.size()) {
    return ZX_ERR_INVALID_ARGS;
  }

  return i2c_devs_[bus_id].Transact(rws, count);
}

zx_status_t AmlI2cDev::Transact(const i2c_impl_op_t* rws, size_t count) const {
  for (size_t i = 0; i < count; ++i) {
    zx_status_t status = SetTargetAddr(rws[i].address);
    if (status != ZX_OK) {
      return status;
    }
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
  ddk::PDev pdev(parent);
  if (!pdev.is_valid()) {
    pdev = ddk::PDev(parent, "pdev");
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

  if (info.mmio_count != info.irq_count) {
    zxlogf(ERROR, "mmio_count %u does not match irq_count %u", info.mmio_count, info.irq_count);
    return ZX_ERR_INVALID_ARGS;
  }

  std::unique_ptr<aml_i2c_delay_values[]> clock_delays;

  size_t metadata_size;
  status = device_get_metadata_size(parent, DEVICE_METADATA_PRIVATE, &metadata_size);
  if (status != ZX_OK) {
    metadata_size = 0;
  } else if (metadata_size != (info.mmio_count * sizeof(clock_delays[0]))) {
    zxlogf(ERROR, "invalid metadata size");
    return ZX_ERR_INVALID_ARGS;
  }

  if (metadata_size > 0) {
    clock_delays = std::make_unique<aml_i2c_delay_values[]>(info.mmio_count);

    size_t actual;
    status = device_get_metadata(parent, DEVICE_METADATA_PRIVATE, clock_delays.get(), metadata_size,
                                 &actual);
    if (status != ZX_OK) {
      zxlogf(ERROR, "device_get_metadata failed");
      return status;
    }
    if (actual != metadata_size) {
      zxlogf(ERROR, "metadata size mismatch");
      return ZX_ERR_INTERNAL;
    }
  }

  fbl::Array i2c_devs(new AmlI2cDev[info.mmio_count], info.mmio_count);
  for (unsigned i = 0; i < i2c_devs.size(); i++) {
    status =
        i2c_devs[i].Init(i, metadata_size > 0 ? clock_delays[i] : aml_i2c_delay_values{0, 0}, pdev);
    if (status != ZX_OK) {
      return status;
    }
  }

  auto i2c = std::make_unique<AmlI2c>(parent, std::move(i2c_devs));

  status = i2c->DdkAdd("aml-i2c");
  if (status != ZX_OK) {
    zxlogf(ERROR, "device_add failed");
    return status;
  }

  [[maybe_unused]] auto* unused = i2c.release();
  return status;
}

}  // namespace aml_i2c

static zx_driver_ops_t aml_i2c_driver_ops = {
    .version = DRIVER_OPS_VERSION,
    .bind = aml_i2c::AmlI2c::Bind,
};

ZIRCON_DRIVER(aml_i2c, aml_i2c_driver_ops, "zircon", "0.1");
