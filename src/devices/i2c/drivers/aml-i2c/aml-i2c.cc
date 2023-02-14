// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "aml-i2c.h"

#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/trace/event.h>
#include <lib/zx/profile.h>
#include <lib/zx/thread.h>
#include <zircon/status.h>
#include <zircon/syscalls.h>
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

static zx_status_t aml_i2c_set_target_addr(aml_i2c_dev_t* dev, uint16_t addr) {
  addr &= 0x7f;
  uint32_t reg = MmioRead32(&dev->virt_regs->target_addr);
  reg = reg & ~0xff;
  reg = reg | ((addr << 1) & 0xff);
  MmioWrite32(reg, &dev->virt_regs->target_addr);

  return ZX_OK;
}

static int aml_i2c_irq_thread(void* arg) {
  auto* dev = reinterpret_cast<aml_i2c_dev_t*>(arg);
  zx_status_t status;

  while (1) {
    status = zx_interrupt_wait(dev->irq, nullptr);
    if (status == ZX_ERR_CANCELED) {
      break;
    }
    if (status != ZX_OK) {
      zxlogf(DEBUG, "i2c: interrupt error");
      continue;
    }
    uint32_t reg = MmioRead32(&dev->virt_regs->control);
    if (reg & AML_I2C_CONTROL_REG_ERR) {
      zx_object_signal(dev->event, 0, I2C_ERROR_SIGNAL);
    } else {
      zx_object_signal(dev->event, 0, I2C_TXN_COMPLETE_SIGNAL);
    }
  }
  return ZX_OK;
}

#if 0
static zx_status_t aml_i2c_dumpstate(aml_i2c_dev_t* dev) {
  printf("control reg      : %08x\n", MmioRead32(&dev->virt_regs->control));
  printf("target addr reg  : %08x\n", MmioRead32(&dev->virt_regs->target_addr));
  printf("token list0 reg  : %08x\n", MmioRead32(&dev->virt_regs->token_list_0));
  printf("token list1 reg  : %08x\n", MmioRead32(&dev->virt_regs->token_list_1));
  printf("token wdata0     : %08x\n", MmioRead32(&dev->virt_regs->token_wdata_0));
  printf("token wdata1     : %08x\n", MmioRead32(&dev->virt_regs->token_wdata_1));
  printf("token rdata0     : %08x\n", MmioRead32(&dev->virt_regs->token_rdata_0));
  printf("token rdata1     : %08x\n", MmioRead32(&dev->virt_regs->token_rdata_1));

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

static zx_status_t aml_i2c_start_xfer(aml_i2c_dev_t* dev) {
  // First have to clear the start bit before setting (RTFM)
  MmioClearBits32(AML_I2C_CONTROL_REG_START, &dev->virt_regs->control);
  MmioSetBits32(AML_I2C_CONTROL_REG_START, &dev->virt_regs->control);
  return ZX_OK;
}

static zx_status_t aml_i2c_wait_event(aml_i2c_dev_t* dev, uint32_t sig_mask) {
  zx_time_t deadline = zx_deadline_after(dev->timeout);
  uint32_t observed;
  sig_mask |= I2C_ERROR_SIGNAL;
  zx_status_t status = zx_object_wait_one(dev->event, sig_mask, deadline, &observed);
  if (status != ZX_OK) {
    return status;
  }
  zx_object_signal(dev->event, observed, 0);
  if (observed & I2C_ERROR_SIGNAL)
    return ZX_ERR_TIMED_OUT;
  return ZX_OK;
}

static zx_status_t aml_i2c_write(aml_i2c_dev_t* dev, const uint8_t* buff, uint32_t len, bool stop) {
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

    MmioWrite32(token_reg & 0xffffffff, &dev->virt_regs->token_list_0);
    token_reg = token_reg >> 32;
    MmioWrite32(token_reg & 0xffffffff, &dev->virt_regs->token_list_1);

    uint64_t wdata = 0;
    for (uint32_t i = 0; i < tx_size; i++) {
      wdata |= static_cast<uint64_t>(buff[i]) << (8 * i);
    }

    MmioWrite32(wdata & 0xffffffff, &dev->virt_regs->token_wdata_0);
    MmioWrite32((wdata >> 32) & 0xffffffff, &dev->virt_regs->token_wdata_1);

    aml_i2c_start_xfer(dev);
    // while (dev->virt_regs->control & 0x4) ;;    // wait for idle
    zx_status_t status = aml_i2c_wait_event(dev, I2C_TXN_COMPLETE_SIGNAL);
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

static zx_status_t aml_i2c_read(aml_i2c_dev_t* dev, uint8_t* buff, uint32_t len, bool stop) {
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

    MmioWrite32(token_reg & 0xffffffff, &dev->virt_regs->token_list_0);
    token_reg = token_reg >> 32;
    MmioWrite32(token_reg & 0xffffffff, &dev->virt_regs->token_list_1);

    // clear registers to prevent data leaking from last xfer
    MmioWrite32(0, &dev->virt_regs->token_rdata_0);
    MmioWrite32(0, &dev->virt_regs->token_rdata_1);

    aml_i2c_start_xfer(dev);

    zx_status_t status = aml_i2c_wait_event(dev, I2C_TXN_COMPLETE_SIGNAL);
    if (status != ZX_OK) {
      return status;
    }

    // while (dev->virt_regs->control & 0x4) ;;    // wait for idle

    uint64_t rdata;
    rdata = MmioRead32(&dev->virt_regs->token_rdata_0);
    rdata |= static_cast<uint64_t>(MmioRead32(&dev->virt_regs->token_rdata_1)) << 32;

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
  timeout = ZX_SEC(1);

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
  irq = irq_.get();

  status = zx::event::create(0, &event_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "zx_event_create failed: %s", zx_status_get_string(status));
    return status;
  }
  event = event_.get();

  thrd_create_with_name(&irqthrd, aml_i2c_irq_thread, this, "i2c_irq_thread");

  // Set profile for IRQ thread.
  // TODO(fxbug.dev/40858): Migrate to the role-based API when available, instead of hard
  // coding parameters.
  const zx_duration_t capacity = ZX_USEC(20);
  const zx_duration_t deadline = ZX_USEC(100);
  const zx_duration_t period = deadline;

  zx::profile irq_profile;
  status = device_get_deadline_profile(nullptr, capacity, deadline, period, "aml_i2c_irq_thread",
                                       irq_profile.reset_and_get_address());
  if (status != ZX_OK) {
    zxlogf(WARNING, "Failed to get deadline profile: %s", zx_status_get_string(status));
  } else {
    zx::unowned_thread thread(thrd_get_zx_handle(irqthrd));
    status = thread->set_profile(irq_profile, 0);
    if (status != ZX_OK) {
      zxlogf(WARNING, "Failed to apply deadline profile to IRQ thread: %s",
             zx_status_get_string(status));
    }
  }

  return ZX_OK;
}

uint32_t aml_i2c_get_bus_count(void* ctx) {
  auto* i2c = reinterpret_cast<aml_i2c_t*>(ctx);

  return i2c->dev_count;
}

uint32_t aml_i2c_get_bus_base(void* ctx) { return 0; }

zx_status_t aml_i2c_get_max_transfer_size(void* ctx, uint32_t bus_id, size_t* out_size) {
  *out_size = AML_I2C_MAX_TRANSFER;
  return ZX_OK;
}

zx_status_t aml_i2c_set_bitrate(void* ctx, uint32_t bus_id, uint32_t bitrate) {
  // TODO(hollande,voydanoff) implement this
  return ZX_ERR_NOT_SUPPORTED;
}

zx_status_t aml_i2c_transact(void* ctx, uint32_t bus_id, const i2c_impl_op_t* rws, size_t count) {
  TRACE_DURATION("i2c", "aml-i2c Transact");
  size_t i;
  for (i = 0; i < count; ++i) {
    if (rws[i].data_size > AML_I2C_MAX_TRANSFER) {
      return ZX_ERR_OUT_OF_RANGE;
    }
  }
  auto* i2c = reinterpret_cast<aml_i2c_t*>(ctx);
  if (bus_id >= i2c->dev_count) {
    return ZX_ERR_INVALID_ARGS;
  }
  aml_i2c_dev_t* dev = &i2c->i2c_devs[bus_id];

  zx_status_t status = ZX_OK;
  for (i = 0; i < count; ++i) {
    status = aml_i2c_set_target_addr(dev, rws[i].address);
    if (status != ZX_OK) {
      return status;
    }
    if (rws[i].is_read) {
      status = aml_i2c_read(dev, rws[i].data_buffer, static_cast<uint32_t>(rws[i].data_size),
                            rws[i].stop);
    } else {
      status = aml_i2c_write(dev, rws[i].data_buffer, static_cast<uint32_t>(rws[i].data_size),
                             rws[i].stop);
    }
    if (status != ZX_OK) {
      return status;  // TODO(andresoportus) release the bus
    }
  }

  return status;
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
