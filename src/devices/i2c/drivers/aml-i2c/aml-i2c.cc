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

#include "src/devices/i2c/drivers/aml-i2c/aml_i2c_bind.h"

#define I2C_ERROR_SIGNAL ZX_USER_SIGNAL_0
#define I2C_TXN_COMPLETE_SIGNAL ZX_USER_SIGNAL_1

#define AML_I2C_CONTROL_REG_START (uint32_t)(1 << 0)
#define AML_I2C_CONTROL_REG_ACK_IGNORE (uint32_t)(1 << 1)
#define AML_I2C_CONTROL_REG_STATUS (uint32_t)(1 << 2)
#define AML_I2C_CONTROL_REG_ERR (uint32_t)(1 << 3)

// There is a separate set of bits in the control register (QTR_CLK_EXT) that
// extends this field to 12 bits. Only expose the main 10-bit field in order to
// simplify the driver logic (delay values for normal bus frequencies won't use
// anywhere near 10 bits anyway).
#define AML_I2C_CONTROL_REG_QTR_CLK_DLY_MAX 0x3ff
#define AML_I2C_CONTROL_REG_QTR_CLK_DLY_SHIFT 12
#define AML_I2C_CONTROL_REG_QTR_CLK_DLY_MASK \
  (uint32_t)(AML_I2C_CONTROL_REG_QTR_CLK_DLY_MAX << AML_I2C_CONTROL_REG_QTR_CLK_DLY_SHIFT)

#define AML_I2C_TARGET_ADDR_REG_USE_CNTL_SCL_LOW (1 << 28)
#define AML_I2C_TARGET_ADDR_REG_SCL_LOW_DELAY_MAX 0xfff
#define AML_I2C_TARGET_ADDR_REG_SCL_LOW_DELAY_SHIFT 16

#define AML_I2C_MAX_TRANSFER 512

namespace aml_i2c {

struct aml_i2c_regs_t {
  uint32_t control;
  uint32_t target_addr;
  uint32_t token_list_0;
  uint32_t token_list_1;
  uint32_t token_wdata_0;
  uint32_t token_wdata_1;
  uint32_t token_rdata_0;
  uint32_t token_rdata_1;
} __PACKED;

enum aml_i2c_token_t : uint64_t {
  TOKEN_END,
  TOKEN_START,
  TOKEN_TARGET_ADDR_WR,
  TOKEN_TARGET_ADDR_RD,
  TOKEN_DATA,
  TOKEN_DATA_LAST,
  TOKEN_STOP
};

zx_status_t AmlI2cDev::SetTargetAddr(uint16_t addr) const {
  addr &= 0x7f;
  uint32_t reg = MmioRead32(&virt_regs->target_addr);
  reg = reg & ~0xff;
  reg = reg | ((addr << 1) & 0xff);
  MmioWrite32(reg, &virt_regs->target_addr);

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
    uint32_t reg = MmioRead32(&virt_regs->control);
    if (reg & AML_I2C_CONTROL_REG_ERR) {
      event_.signal(0, I2C_ERROR_SIGNAL);
    } else {
      event_.signal(0, I2C_TXN_COMPLETE_SIGNAL);
    }
  }
  return ZX_OK;
}

#if 0
zx_status_t AmlI2cDev::DumpState() {
  printf("control reg      : %08x\n", MmioRead32(&virt_regs->control));
  printf("target addr reg  : %08x\n", MmioRead32(&virt_regs->target_addr));
  printf("token list0 reg  : %08x\n", MmioRead32(&virt_regs->token_list_0));
  printf("token list1 reg  : %08x\n", MmioRead32(&virt_regs->token_list_1));
  printf("token wdata0     : %08x\n", MmioRead32(&virt_regs->token_wdata_0));
  printf("token wdata1     : %08x\n", MmioRead32(&virt_regs->token_wdata_1));
  printf("token rdata0     : %08x\n", MmioRead32(&virt_regs->token_rdata_0));
  printf("token rdata1     : %08x\n", MmioRead32(&virt_regs->token_rdata_1));

  return ZX_OK;
}
#endif

static inline void MmioClearBits32(uint32_t bits, MMIO_PTR volatile uint32_t* buffer) {
  uint32_t reg = MmioRead32(buffer);
  reg &= ~bits;
  MmioWrite32(reg, buffer);
}

static inline void MmioSetBits32(uint32_t bits, MMIO_PTR volatile uint32_t* buffer) {
  uint32_t reg = MmioRead32(buffer);
  reg |= bits;
  MmioWrite32(reg, buffer);
}

zx_status_t AmlI2cDev::StartXfer() const {
  // First have to clear the start bit before setting (RTFM)
  MmioClearBits32(AML_I2C_CONTROL_REG_START, &virt_regs->control);
  MmioSetBits32(AML_I2C_CONTROL_REG_START, &virt_regs->control);
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
  uint32_t token_num = 0;
  uint64_t token_reg = 0;

  token_reg |= TOKEN_START << (4 * (token_num++));
  token_reg |= TOKEN_TARGET_ADDR_WR << (4 * (token_num++));

  while (len > 0) {
    bool is_last_iter = len <= 8;
    uint32_t tx_size = is_last_iter ? len : 8;
    for (uint32_t i = 0; i < tx_size; i++) {
      token_reg |= TOKEN_DATA << (4 * (token_num++));
    }

    if (is_last_iter && stop) {
      token_reg |= TOKEN_STOP << (4 * (token_num++));
    }

    MmioWrite32(token_reg & 0xffffffff, &virt_regs->token_list_0);
    token_reg = token_reg >> 32;
    MmioWrite32(token_reg & 0xffffffff, &virt_regs->token_list_1);

    uint64_t wdata = 0;
    for (uint32_t i = 0; i < tx_size; i++) {
      wdata |= static_cast<uint64_t>(buff[i]) << (8 * i);
    }

    MmioWrite32(wdata & 0xffffffff, &virt_regs->token_wdata_0);
    MmioWrite32((wdata >> 32) & 0xffffffff, &virt_regs->token_wdata_1);

    StartXfer();
    // while (virt_regs->control & 0x4) ;;    // wait for idle
    zx_status_t status = WaitEvent(I2C_TXN_COMPLETE_SIGNAL);
    if (status != ZX_OK) {
      return status;
    }

    len -= tx_size;
    buff += tx_size;
    token_num = 0;
    token_reg = 0;
  }

  return ZX_OK;
}

zx_status_t AmlI2cDev::Read(uint8_t* buff, uint32_t len, bool stop) const {
  ZX_DEBUG_ASSERT(len <= AML_I2C_MAX_TRANSFER);
  TRACE_DURATION("i2c", "aml-i2c Read");
  uint32_t token_num = 0;
  uint64_t token_reg = 0;

  token_reg |= TOKEN_START << (4 * (token_num++));
  token_reg |= TOKEN_TARGET_ADDR_RD << (4 * (token_num++));

  while (len > 0) {
    bool is_last_iter = len <= 8;
    uint32_t rx_size = is_last_iter ? len : 8;

    for (uint32_t i = 0; i < (rx_size - 1); i++) {
      token_reg |= TOKEN_DATA << (4 * (token_num++));
    }
    if (is_last_iter) {
      token_reg |= TOKEN_DATA_LAST << (4 * (token_num++));
      if (stop) {
        token_reg |= TOKEN_STOP << (4 * (token_num++));
      }
    } else {
      token_reg |= TOKEN_DATA << (4 * (token_num++));
    }

    MmioWrite32(token_reg & 0xffffffff, &virt_regs->token_list_0);
    token_reg = token_reg >> 32;
    MmioWrite32(token_reg & 0xffffffff, &virt_regs->token_list_1);

    // clear registers to prevent data leaking from last xfer
    MmioWrite32(0, &virt_regs->token_rdata_0);
    MmioWrite32(0, &virt_regs->token_rdata_1);

    StartXfer();

    zx_status_t status = WaitEvent(I2C_TXN_COMPLETE_SIGNAL);
    if (status != ZX_OK) {
      return status;
    }

    // while (virt_regs->control & 0x4) ;;    // wait for idle

    uint64_t rdata;
    rdata = MmioRead32(&virt_regs->token_rdata_0);
    rdata |= static_cast<uint64_t>(MmioRead32(&virt_regs->token_rdata_1)) << 32;

    for (uint32_t i = 0; i < rx_size; i++, rdata >>= 8) {
      buff[i] = rdata & 0xff;
    }

    len -= rx_size;
    buff += rx_size;
    token_num = 0;
    token_reg = 0;
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

  virt_regs = reinterpret_cast<MMIO_PTR aml_i2c_regs_t*>(regs_iobuff_->get());

  if (delay.quarter_clock_delay > AML_I2C_CONTROL_REG_QTR_CLK_DLY_MAX ||
      delay.clock_low_delay > AML_I2C_TARGET_ADDR_REG_SCL_LOW_DELAY_MAX) {
    zxlogf(ERROR, "invalid clock delay");
    return ZX_ERR_INVALID_ARGS;
  }

  if (delay.quarter_clock_delay > 0) {
    uint32_t control = MmioRead32(&virt_regs->control);
    control &= ~AML_I2C_CONTROL_REG_QTR_CLK_DLY_MASK;
    control |= delay.quarter_clock_delay << AML_I2C_CONTROL_REG_QTR_CLK_DLY_SHIFT;
    MmioWrite32(control, &virt_regs->control);
  }

  if (delay.clock_low_delay > 0) {
    uint32_t reg = delay.clock_low_delay << AML_I2C_TARGET_ADDR_REG_SCL_LOW_DELAY_SHIFT;
    reg |= AML_I2C_TARGET_ADDR_REG_USE_CNTL_SCL_LOW;
    MmioWrite32(reg, &virt_regs->target_addr);
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
