// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "imx8m-gpio.h"

#include <fidl/fuchsia.hardware.platform.bus/cpp/driver/fidl.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <lib/device-protocol/pdev-fidl.h>

#include <fbl/algorithm.h>
#include <fbl/alloc_checker.h>

namespace {

// Interrupt port key range is 0 to (imx8m::kMaxGpioPorts * imx8m::kInterruptsPerPort - 1)
// Below key is used to tell the interrupt thread to exit when the driver is shutting down.
constexpr uint32_t kPortKeyTerminate = imx8m::kMaxGpioPorts * imx8m::kInterruptsPerPort;

}  // namespace

namespace gpio {

zx_status_t Imx8mGpio::Create(void* ctx, zx_device_t* parent) {
  imx8m::PinConfigMetadata pinconfig_metadata = {};
  size_t actual;
  zx_status_t status = device_get_metadata(parent, DEVICE_METADATA_PRIVATE, &pinconfig_metadata,
                                           sizeof(pinconfig_metadata), &actual);
  if (status != ZX_OK) {
    zxlogf(ERROR, "Failed to get metadata: %s", zx_status_get_string(status));
    return status;
  }
  if (actual != sizeof(pinconfig_metadata)) {
    zxlogf(ERROR, "Unexpected metadata size");
    return ZX_ERR_INTERNAL;
  }

  ddk::PDevFidl pdev(parent);
  if (!pdev.is_valid()) {
    zxlogf(ERROR, "Failed to get ZX_PROTOCOL_PLATFORM_DEVICE");
    return ZX_ERR_NO_RESOURCES;
  }

  pdev_device_info_t device_info = {};
  if ((status = pdev.GetDeviceInfo(&device_info)) != ZX_OK) {
    zxlogf(ERROR, "Failed to get device info: %s", zx_status_get_string(status));
    return status;
  }

  constexpr uint32_t kPinconfigMmioCount = 1;
  const uint32_t gpio_mmio_count = device_info.mmio_count - kPinconfigMmioCount;

  if (gpio_mmio_count > imx8m::kMaxGpioPorts) {
    zxlogf(ERROR, "Too many GPIO MMIOs specified");
    return ZX_ERR_INTERNAL;
  }

  if (device_info.irq_count != (gpio_mmio_count * imx8m::kInterruptsPerPort)) {
    zxlogf(ERROR, "Too many interrupts specified");
    return ZX_ERR_INTERNAL;
  }

  fbl::AllocChecker ac;

  std::optional<ddk::MmioBuffer> pinconfig_mmio;
  if ((status = pdev.MapMmio(0, &pinconfig_mmio)) != ZX_OK) {
    zxlogf(ERROR, "Failed to map pinconfig MMIO: %s", zx_status_get_string(status));
    return status;
  }

  fbl::Vector<ddk::MmioBuffer> gpio_mmios;
  gpio_mmios.reserve(gpio_mmio_count, &ac);
  if (!ac.check()) {
    zxlogf(ERROR, "Allocation failed");
    return ZX_ERR_NO_RESOURCES;
  }

  for (uint32_t i = kPinconfigMmioCount; i < kPinconfigMmioCount + gpio_mmio_count; i++) {
    std::optional<ddk::MmioBuffer> mmio;
    if ((status = pdev.MapMmio(i, &mmio)) != ZX_OK) {
      zxlogf(ERROR, "Failed to map GPIO MMIO: %s", zx_status_get_string(status));
      return status;
    }
    gpio_mmios.push_back(*std::move(mmio));
  }

  fbl::Array<zx::interrupt> port_interrupts(new (&ac) zx::interrupt[device_info.irq_count],
                                            device_info.irq_count);
  if (!ac.check()) {
    zxlogf(ERROR, "Allocation failed");
    return ZX_ERR_NO_RESOURCES;
  }

  for (uint32_t i = 0; i < port_interrupts.size(); i++) {
    zx::interrupt interrupt;
    if ((status = pdev.GetInterrupt(i, &interrupt)) != ZX_OK) {
      zxlogf(ERROR, "Failed to get interrupt: %s", zx_status_get_string(status));
      return status;
    }
    port_interrupts[i] = std::move(interrupt);
  }

  auto device = fbl::make_unique_checked<Imx8mGpio>(&ac, parent, *std::move(pinconfig_mmio),
                                                    std::move(gpio_mmios),
                                                    std::move(port_interrupts), pinconfig_metadata);
  if (!ac.check()) {
    zxlogf(ERROR, "Failed to allocate device memory");
    return ZX_ERR_NO_MEMORY;
  }

  if ((status = device->Init()) != ZX_OK) {
    zxlogf(ERROR, "Init failed: %s", zx_status_get_string(status));
    return status;
  }

  if ((status = device->Bind()) != ZX_OK) {
    zxlogf(ERROR, "Bind failed: %s", zx_status_get_string(status));
    device->Shutdown();
    return status;
  }

  [[maybe_unused]] auto* placeholder = device.release();

  return ZX_OK;
}

zx_status_t Imx8mGpio::Init() {
  // Perform pin muxing and configuration
  for (uint32_t i = 0; i < pinconfig_metadata_.pin_config_entry_count; i++) {
    pinconfig_mmio_.Write32(pinconfig_metadata_.pin_config_entry[i].mux_val,
                            pinconfig_metadata_.pin_config_entry[i].mux_reg_offset);
    pinconfig_mmio_.Write32(pinconfig_metadata_.pin_config_entry[i].conf_val,
                            pinconfig_metadata_.pin_config_entry[i].conf_reg_offset);
    // It is not necessary to have input reg for each pin so write
    // input reg only if its offset it set.
    if (pinconfig_metadata_.pin_config_entry[i].input_reg_offset) {
      pinconfig_mmio_.Write32(pinconfig_metadata_.pin_config_entry[i].input_val,
                              pinconfig_metadata_.pin_config_entry[i].input_reg_offset);
    }
  }

  zx_status_t status = zx::port::create(ZX_PORT_BIND_TO_INTERRUPT, &port_);
  if (status != ZX_OK) {
    zxlogf(ERROR, "zx_port_create failed %s", zx_status_get_string(status));
    return status;
  }

  for (const ddk::MmioBuffer& gpio_mmio : gpio_mmios_) {
    // Disable interrupt
    gpio_mmio.Write32(0x0, imx8m::kGpioInterruptMaskReg);
    // Clear interrupt status
    gpio_mmio.Write32(~0x0, imx8m::kGpioInterruptStatusReg);
  }

  uint32_t port_key = 0;
  for (const zx::interrupt& port_interrupt : port_interrupts_) {
    status = port_interrupt.bind(port_, port_key++, 0);
    if (status != ZX_OK) {
      zxlogf(ERROR, "zx_interrupt_bind failed %s", zx_status_get_string(status));
      return status;
    }
  }

  const size_t interrupt_count =
      imx8m::kInterruptsPerPort * imx8m::kGpiosPerInterrupt * gpio_mmios_.size();

  fbl::AllocChecker ac;
  gpio_interrupts_ = fbl::Array(new (&ac) zx::interrupt[interrupt_count], interrupt_count);
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  auto cb = [](void* arg) -> int { return reinterpret_cast<Imx8mGpio*>(arg)->Thread(); };
  int rc = thrd_create_with_name(&thread_, cb, this, "imx8m-gpio-thread");
  if (rc != thrd_success) {
    return ZX_ERR_INTERNAL;
  }

  return ZX_OK;
}

zx_status_t Imx8mGpio::Bind() {
  auto status = DdkAdd(ddk::DeviceAddArgs("imx8m-gpio")
                           .set_proto_id(ddk_proto_id_)
                           .forward_metadata(parent(), DEVICE_METADATA_GPIO_PINS));
  if (status != ZX_OK) {
    zxlogf(ERROR, "imx8m-gpio::Create: DdkAdd failed");
    return status;
  }

  return ZX_OK;
}

int Imx8mGpio::Thread() {
  while (1) {
    zx_port_packet_t packet;
    zx_status_t status = port_.wait(zx::time::infinite(), &packet);
    if (status != ZX_OK) {
      zxlogf(ERROR, "port wait failed: %s", zx_status_get_string(status));
      return thrd_error;
    }

    if (packet.key == kPortKeyTerminate) {
      zxlogf(INFO, "Imx8mGpio thread terminating");
      return thrd_success;
    }
    if (packet.key >= gpio_mmios_.size() * imx8m::kInterruptsPerPort) {
      zxlogf(WARNING, "received interrupt from invalid port");
      continue;
    }

    constexpr uint32_t kInterruptCount = imx8m::kGpiosPerInterrupt * imx8m::kInterruptsPerPort;
    const uint64_t port = packet.key / imx8m::kInterruptsPerPort;
    uint32_t irq = gpio_mmios_[port].Read32(imx8m::kGpioInterruptStatusReg);
    uint32_t index, index_end;
    const uint32_t interrupt_mask = gpio_mmios_[port].Read32(imx8m::kGpioInterruptMaskReg);

    if (packet.key % imx8m::kInterruptsPerPort) {
      irq &= 0xffff0000;
      index = imx8m::kGpiosPerInterrupt;
      index_end = imx8m::kInterruptsPerPort * imx8m::kInterruptsPerPort - 1;
    } else {
      irq &= 0x0000ffff;
      index = 0;
      index_end = imx8m::kGpiosPerInterrupt - 1;
    }

    for (uint32_t i = index; i <= index_end; i++) {
      if (irq & (1 << i)) {
        // Notify if interrupt is enabled for the GPIO pin.
        if (interrupt_mask & (1 << i)) {
          status = gpio_interrupts_[(port * kInterruptCount) + i].trigger(
              0, zx::time(packet.interrupt.timestamp));
          if (status != ZX_OK) {
            zxlogf(ERROR, "zx_interrupt_trigger failed %s", zx_status_get_string(status));
          }
        }
      }
    }

    // Clear interrupts
    gpio_mmios_[port].Write32(irq, imx8m::kGpioInterruptStatusReg);

    port_interrupts_[packet.key].ack();
  }

  return thrd_success;
}

zx_status_t Imx8mGpio::ValidateGpioPin(const uint32_t port, const uint32_t pin) {
  if (port >= gpio_mmios_.size()) {
    return ZX_ERR_OUT_OF_RANGE;
  }

  if (pin >= pinconfig_metadata_.port_info[port].pin_count) {
    return ZX_ERR_OUT_OF_RANGE;
  }

  return ZX_OK;
}

zx_status_t Imx8mGpio::GpioImplConfigIn(uint32_t index, uint32_t flags) {
  // Pull up/down should be set-up using pinconfig
  // metadata from board gpio init function.
  if ((flags & GPIO_PULL_MASK) != GPIO_NO_PULL) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  const uint32_t port = index / imx8m::kMaxGpiosPerPort;
  const uint32_t pin = index % imx8m::kMaxGpiosPerPort;

  zx_status_t status = ValidateGpioPin(port, pin);
  if (status != ZX_OK) {
    return status;
  }

  gpio_mmios_[port].ClearBit<uint32_t>(pin, imx8m::kGpioDirReg);

  return status;
}

zx_status_t Imx8mGpio::GpioImplConfigOut(uint32_t index, uint8_t initial_value) {
  const uint32_t port = index / imx8m::kMaxGpiosPerPort;
  const uint32_t pin = index % imx8m::kMaxGpiosPerPort;

  zx_status_t status = ValidateGpioPin(port, pin);
  if (status != ZX_OK) {
    return status;
  }

  gpio_mmios_[port].ModifyBit<uint32_t>(initial_value, pin, imx8m::kGpioDataReg);
  gpio_mmios_[port].SetBit<uint32_t>(pin, imx8m::kGpioDirReg);

  return status;
}

zx_status_t Imx8mGpio::GpioImplSetAltFunction(uint32_t index, uint64_t function) {
  return ZX_ERR_NOT_SUPPORTED;
}

zx_status_t Imx8mGpio::GpioImplSetDriveStrength(uint32_t index, uint64_t ua,
                                                uint64_t* out_actual_ua) {
  return ZX_ERR_NOT_SUPPORTED;
}

zx_status_t Imx8mGpio::GpioImplGetDriveStrength(uint32_t index, uint64_t* out_value) {
  return ZX_ERR_NOT_SUPPORTED;
}

zx_status_t Imx8mGpio::GpioImplRead(uint32_t index, uint8_t* out_value) {
  const uint32_t port = index / imx8m::kMaxGpiosPerPort;
  const uint32_t pin = index % imx8m::kMaxGpiosPerPort;

  zx_status_t status = ValidateGpioPin(port, pin);
  if (status != ZX_OK) {
    return status;
  }

  *out_value = static_cast<uint8_t>(gpio_mmios_[port].GetBit<uint32_t>(pin, imx8m::kGpioDataReg));

  return status;
}

zx_status_t Imx8mGpio::GpioImplWrite(uint32_t index, uint8_t value) {
  const uint32_t port = index / imx8m::kMaxGpiosPerPort;
  const uint32_t pin = index % imx8m::kMaxGpiosPerPort;

  zx_status_t status = ValidateGpioPin(port, pin);
  if (status != ZX_OK) {
    return status;
  }

  gpio_mmios_[port].ModifyBit<uint32_t>(value, pin, imx8m::kGpioDataReg);

  return status;
}

zx_status_t Imx8mGpio::GpioImplGetInterrupt(uint32_t index, uint32_t flags,
                                            zx::interrupt* out_irq) {
  const uint32_t port = index / imx8m::kMaxGpiosPerPort;
  const uint32_t pin = index % imx8m::kMaxGpiosPerPort;

  zx_status_t status = ValidateGpioPin(port, pin);
  if (status != ZX_OK) {
    return status;
  }

  if (gpio_mmios_[port].Read32(imx8m::kGpioInterruptMaskReg) & (1 << pin)) {
    zxlogf(ERROR, "interrupt %u already exists", index);
    return ZX_ERR_ALREADY_EXISTS;
  }

  zx::interrupt irq;
  status = zx::interrupt::create(zx::resource(), index, ZX_INTERRUPT_VIRTUAL, &irq);
  if (status != ZX_OK) {
    zxlogf(ERROR, "zx::interrupt::create failed %s", zx_status_get_string(status));
    return status;
  }
  status = irq.duplicate(ZX_RIGHT_SAME_RIGHTS, out_irq);
  if (status != ZX_OK) {
    zxlogf(ERROR, "interrupt.duplicate failed %s", zx_status_get_string(status));
    return status;
  }

  const zx_off_t offset = (pin < imx8m::kGpiosPerInterrupt) ? imx8m::kGpioInterruptConfReg1
                                                            : imx8m::kGpioInterruptConfReg2;
  const size_t shift =
      (pin < imx8m::kGpiosPerInterrupt)
          ? (pin * imx8m::kGpioInterruptConfBitCount)
          : ((pin - imx8m::kGpiosPerInterrupt) * imx8m::kGpioInterruptConfBitCount);

  gpio_mmios_[port].ClearBit<uint32_t>(pin, imx8m::kGpioEdgeSelectReg);

  switch (flags & ZX_INTERRUPT_MODE_MASK) {
    case ZX_INTERRUPT_MODE_EDGE_LOW:
      gpio_mmios_[port].ModifyBits<uint32_t>(imx8m::kGpioInterruptFallingEdge, shift,
                                             imx8m::kGpioInterruptConfBitCount, offset);
      break;
    case ZX_INTERRUPT_MODE_EDGE_HIGH:
      gpio_mmios_[port].ModifyBits<uint32_t>(imx8m::kGpioInterruptRisingEdge, shift,
                                             imx8m::kGpioInterruptConfBitCount, offset);
      break;
    case ZX_INTERRUPT_MODE_LEVEL_LOW:
      gpio_mmios_[port].ModifyBits<uint32_t>(imx8m::kGpioInterruptLowLevel, shift,
                                             imx8m::kGpioInterruptConfBitCount, offset);
      break;
    case ZX_INTERRUPT_MODE_LEVEL_HIGH:
      gpio_mmios_[port].ModifyBits<uint32_t>(imx8m::kGpioInterruptHighLevel, shift,
                                             imx8m::kGpioInterruptConfBitCount, offset);
      break;
    case ZX_INTERRUPT_MODE_EDGE_BOTH:
      gpio_mmios_[port].SetBit<uint32_t>(pin, imx8m::kGpioEdgeSelectReg);
      break;
    default:
      return ZX_ERR_INVALID_ARGS;
  }

  gpio_interrupts_[index] = std::move(irq);

  // Clear interrupt status
  gpio_mmios_[port].Write32(1 << pin, imx8m::kGpioInterruptStatusReg);
  // Enable interrupt
  gpio_mmios_[port].SetBit<uint32_t>(pin, imx8m::kGpioInterruptMaskReg);

  zxlogf(DEBUG, "INT %u enabled", index);

  return ZX_OK;
}

zx_status_t Imx8mGpio::GpioImplReleaseInterrupt(uint32_t index) {
  const uint32_t port = index / imx8m::kMaxGpiosPerPort;
  const uint32_t pin = index % imx8m::kMaxGpiosPerPort;

  zx_status_t status = ValidateGpioPin(port, pin);
  if (status != ZX_OK) {
    return status;
  }

  if ((gpio_mmios_[port].Read32(imx8m::kGpioInterruptMaskReg) & (1 << pin)) == 0) {
    return ZX_ERR_BAD_STATE;
  }

  // Disable interrupt
  gpio_mmios_[port].ClearBit<uint32_t>(pin, imx8m::kGpioInterruptMaskReg);
  // Clear interrupt status
  gpio_mmios_[port].Write32(1 << pin, imx8m::kGpioInterruptStatusReg);

  gpio_interrupts_[index].destroy();
  gpio_interrupts_[index].reset();

  return ZX_OK;
}

zx_status_t Imx8mGpio::GpioImplSetPolarity(uint32_t index, gpio_polarity_t polarity) {
  return ZX_ERR_NOT_SUPPORTED;
}

void Imx8mGpio::Shutdown() {
  zx_port_packet packet = {kPortKeyTerminate, ZX_PKT_TYPE_USER, ZX_OK, {}};
  port_.queue(&packet);
  thrd_join(thread_, nullptr);
}

void Imx8mGpio::DdkUnbind(ddk::UnbindTxn txn) {
  Shutdown();
  txn.Reply();
}

void Imx8mGpio::DdkRelease() { delete this; }

}  // namespace gpio

static constexpr zx_driver_ops_t imx8m_gpio_driver_ops = []() -> zx_driver_ops_t {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = gpio::Imx8mGpio::Create;
  return ops;
}();

// clang-format off
ZIRCON_DRIVER(imx8m_gpio, imx8m_gpio_driver_ops, "zircon", "0.1");
