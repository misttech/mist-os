// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "aml-gpio.h"

#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>

#include <cstdint>
#include <memory>

#include <fbl/alloc_checker.h>

#include "a1-blocks.h"
#include "a113-blocks.h"
#include "a5-blocks.h"
#include "s905d2-blocks.h"

namespace {

constexpr int kAltFnMax = 15;
constexpr int kMaxPinsInDSReg = 16;
constexpr int kGpioInterruptPolarityShift = 16;
constexpr int kMaxGpioIndex = 255;
constexpr int kBitsPerGpioInterrupt = 8;
constexpr int kBitsPerFilterSelect = 4;

uint32_t GetUnusedIrqIndex(uint8_t status) {
  // First isolate the rightmost 0-bit
  auto zero_bit_set = static_cast<uint8_t>(~status & (status + 1));
  // Count no. of leading zeros
  return __builtin_ctz(zero_bit_set);
}

// Supported Drive Strengths
enum DriveStrength {
  DRV_500UA,
  DRV_2500UA,
  DRV_3000UA,
  DRV_4000UA,
};

}  // namespace

namespace gpio {

// MMIO indices (based on aml-gpio.c gpio_mmios)
enum {
  MMIO_GPIO = 0,
  MMIO_GPIO_AO = 1,
  MMIO_GPIO_INTERRUPTS = 2,
};

zx_status_t AmlGpio::Create(void* ctx, zx_device_t* parent) {
  zx_status_t status;

  ddk::PDevFidl pdev(parent);
  std::optional<fdf::MmioBuffer> mmio_gpio, mmio_gpio_a0, mmio_interrupt;
  if ((status = pdev.MapMmio(MMIO_GPIO, &mmio_gpio)) != ZX_OK) {
    zxlogf(ERROR, "AmlGpio::Create: MapMmio failed");
    return status;
  }

  if ((status = pdev.MapMmio(MMIO_GPIO_AO, &mmio_gpio_a0)) != ZX_OK) {
    zxlogf(ERROR, "AmlGpio::Create: MapMmio failed");
    return status;
  }

  if ((status = pdev.MapMmio(MMIO_GPIO_INTERRUPTS, &mmio_interrupt)) != ZX_OK) {
    zxlogf(ERROR, "AmlGpio::Create: MapMmio failed");
    return status;
  }

  pdev_device_info_t info;
  if ((status = pdev.GetDeviceInfo(&info)) != ZX_OK) {
    zxlogf(ERROR, "AmlGpio::Create: GetDeviceInfo failed");
    return status;
  }

  if (info.pid == 0) {
    // TODO(https://fxbug.dev/318736574) : Remove and rely only on GetDeviceInfo.
    pdev_board_info_t board_info;
    if ((status = pdev.GetBoardInfo(&board_info)) != ZX_OK) {
      zxlogf(ERROR, "AmlGpio::Create: GetBoardInfo failed");
      return status;
    }
    if (board_info.vid == PDEV_VID_AMLOGIC) {
      info.pid = board_info.pid;
    } else if (board_info.vid == PDEV_VID_GOOGLE) {
      switch (board_info.pid) {
        case PDEV_PID_ASTRO:
          info.pid = PDEV_PID_AMLOGIC_S905D2;
          break;
        case PDEV_PID_SHERLOCK:
          info.pid = PDEV_PID_AMLOGIC_T931;
          break;
        case PDEV_PID_NELSON:
          info.pid = PDEV_PID_AMLOGIC_S905D3;
          break;
        default:
          zxlogf(ERROR, "Unsupported PID 0x%x for VID 0x%x", board_info.pid, board_info.vid);
          return ZX_ERR_INVALID_ARGS;
      }
    } else if (board_info.vid == PDEV_VID_KHADAS) {
      switch (board_info.pid) {
        case PDEV_PID_VIM3:
          info.pid = PDEV_PID_AMLOGIC_A311D;
          break;
        default:
          zxlogf(ERROR, "Unsupported PID 0x%x for VID 0x%x", board_info.pid, board_info.vid);
          return ZX_ERR_INVALID_ARGS;
      }
    } else {
      zxlogf(ERROR, "Unsupported VID 0x%x", board_info.vid);
      return ZX_ERR_INVALID_ARGS;
    }
  }

  const AmlGpioBlock* gpio_blocks;
  const AmlGpioInterrupt* gpio_interrupt;
  size_t block_count;

  switch (info.pid) {
    case PDEV_PID_AMLOGIC_A113:
      gpio_blocks = a113_gpio_blocks;
      block_count = std::size(a113_gpio_blocks);
      gpio_interrupt = &a113_interrupt_block;
      break;
    case PDEV_PID_AMLOGIC_S905D2:
    case PDEV_PID_AMLOGIC_T931:
    case PDEV_PID_AMLOGIC_A311D:
    case PDEV_PID_AMLOGIC_S905D3:
      // S905D2, T931, A311D, S905D3 are identical.
      gpio_blocks = s905d2_gpio_blocks;
      block_count = std::size(s905d2_gpio_blocks);
      gpio_interrupt = &s905d2_interrupt_block;
      break;
    case PDEV_PID_AMLOGIC_A5:
      gpio_blocks = a5_gpio_blocks;
      block_count = std::size(a5_gpio_blocks);
      gpio_interrupt = &a5_interrupt_block;
      break;
    case PDEV_PID_AMLOGIC_A1:
      gpio_blocks = a1_gpio_blocks;
      block_count = std::size(a1_gpio_blocks);
      gpio_interrupt = &a1_interrupt_block;
      break;
    default:
      zxlogf(ERROR, "AmlGpio::Create: unsupported SOC PID %u", info.pid);
      return ZX_ERR_INVALID_ARGS;
  }

  fbl::AllocChecker ac;

  fbl::Array<uint16_t> irq_info(new (&ac) uint16_t[info.irq_count], info.irq_count);
  if (!ac.check()) {
    zxlogf(ERROR, "AmlGpio::Create: irq_info alloc failed");
    return ZX_ERR_NO_MEMORY;
  }
  for (uint32_t i = 0; i < info.irq_count; i++) {
    irq_info[i] = kMaxGpioIndex + 1;
  }  // initialize irq_info

  std::unique_ptr<AmlGpio> device(new (&ac) AmlGpio(
      parent, *std::move(mmio_gpio), *std::move(mmio_gpio_a0), *std::move(mmio_interrupt),
      gpio_blocks, gpio_interrupt, block_count, std::move(info), std::move(irq_info)));
  if (!ac.check()) {
    zxlogf(ERROR, "AmlGpio::Create: device object alloc failed");
    return ZX_ERR_NO_MEMORY;
  }

  {
    fuchsia_hardware_gpioimpl::Service::InstanceHandler handler({
        .device = device->bindings_.CreateHandler(
            device.get(), fdf::Dispatcher::GetCurrent()->get(), fidl::kIgnoreBindingClosure),
    });
    auto result =
        device->outgoing_.AddService<fuchsia_hardware_gpioimpl::Service>(std::move(handler));
    if (result.is_error()) {
      zxlogf(ERROR, "AddService failed: %s", result.status_string());
      return result.error_value();
    }
  }

  auto directory_endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
  if (directory_endpoints.is_error()) {
    return directory_endpoints.status_value();
  }

  {
    auto result = device->outgoing_.Serve(std::move(directory_endpoints->server));
    if (result.is_error()) {
      zxlogf(ERROR, "Failed to serve the outgoing directory: %s", result.status_string());
      return result.error_value();
    }
  }

  std::array<const char*, 1> service_offers{fuchsia_hardware_gpioimpl::Service::Name};
  if (auto status =
          device->DdkAdd(ddk::DeviceAddArgs("aml-gpio")
                             .set_proto_id(ZX_PROTOCOL_GPIO_IMPL)
                             .set_runtime_service_offers(service_offers)
                             .set_outgoing_dir(directory_endpoints->client.TakeChannel())
                             .forward_metadata(parent, DEVICE_METADATA_GPIO_PINS)
                             .forward_metadata(parent, DEVICE_METADATA_GPIO_INIT)
                             .forward_metadata(parent, DEVICE_METADATA_SCHEDULER_ROLE_NAME));
      status != ZX_OK) {
    zxlogf(ERROR, "AmlGpio::Create: DdkAdd failed");
    return status;
  }

  [[maybe_unused]] auto* unused = device.release();

  return ZX_OK;
}

zx_status_t AmlGpio::AmlPinToBlock(const uint32_t pin, const AmlGpioBlock** out_block,
                                   uint32_t* out_pin_index) const {
  ZX_DEBUG_ASSERT(out_block && out_pin_index);

  for (size_t i = 0; i < block_count_; i++) {
    const AmlGpioBlock& gpio_block = gpio_blocks_[i];
    const uint32_t end_pin = gpio_block.start_pin + gpio_block.pin_count;
    if (pin >= gpio_block.start_pin && pin < end_pin) {
      *out_block = &gpio_block;
      *out_pin_index = pin - gpio_block.pin_block + gpio_block.output_shift;
      return ZX_OK;
    }
  }

  return ZX_ERR_NOT_FOUND;
}

void AmlGpio::ConfigIn(fuchsia_hardware_gpioimpl::wire::GpioImplConfigInRequest* request,
                       fdf::Arena& arena, ConfigInCompleter::Sync& completer) {
  zx_status_t status;

  const AmlGpioBlock* block;
  uint32_t pinindex;
  if ((status = AmlPinToBlock(request->index, &block, &pinindex)) != ZX_OK) {
    zxlogf(ERROR, "AmlGpio::GpioImplConfigIn: pin not found %u", request->index);
    return completer.buffer(arena).ReplyError(status);
  }

  const uint32_t pinmask = 1 << pinindex;

  uint32_t regval = mmios_[block->mmio_index].Read32(block->oen_offset * sizeof(uint32_t));
  // Set the GPIO as pull-up or pull-down
  auto pull = request->flags;
  uint32_t pull_reg_val = mmios_[block->mmio_index].Read32(block->pull_offset * sizeof(uint32_t));
  uint32_t pull_en_reg_val =
      mmios_[block->mmio_index].Read32(block->pull_en_offset * sizeof(uint32_t));
  if (pull == fuchsia_hardware_gpio::GpioFlags::kNoPull) {
    pull_en_reg_val &= ~pinmask;
  } else {
    if (pull == fuchsia_hardware_gpio::GpioFlags::kPullUp) {
      pull_reg_val |= pinmask;
    } else {
      pull_reg_val &= ~pinmask;
    }
    pull_en_reg_val |= pinmask;
  }

  mmios_[block->mmio_index].Write32(pull_reg_val, block->pull_offset * sizeof(uint32_t));
  mmios_[block->mmio_index].Write32(pull_en_reg_val, block->pull_en_offset * sizeof(uint32_t));
  regval |= pinmask;
  mmios_[block->mmio_index].Write32(regval, block->oen_offset * sizeof(uint32_t));

  completer.buffer(arena).ReplySuccess();
}

void AmlGpio::ConfigOut(fuchsia_hardware_gpioimpl::wire::GpioImplConfigOutRequest* request,
                        fdf::Arena& arena, ConfigOutCompleter::Sync& completer) {
  zx_status_t status;

  const AmlGpioBlock* block;
  uint32_t pinindex;
  if ((status = AmlPinToBlock(request->index, &block, &pinindex)) != ZX_OK) {
    zxlogf(ERROR, "AmlGpio::GpioImplConfigOut: pin not found %u", request->index);
    return completer.buffer(arena).ReplyError(status);
  }

  const uint32_t pinmask = 1 << pinindex;

  // Set value before configuring for output
  uint32_t regval = mmios_[block->mmio_index].Read32(block->output_offset * sizeof(uint32_t));
  if (request->initial_value) {
    regval |= pinmask;
  } else {
    regval &= ~pinmask;
  }
  mmios_[block->mmio_index].Write32(regval, block->output_offset * sizeof(uint32_t));

  regval = mmios_[block->mmio_index].Read32(block->oen_offset * sizeof(uint32_t));
  regval &= ~pinmask;
  mmios_[block->mmio_index].Write32(regval, block->oen_offset * sizeof(uint32_t));

  completer.buffer(arena).ReplySuccess();
}

// Configure a pin for an alternate function specified by fn
void AmlGpio::SetAltFunction(
    fuchsia_hardware_gpioimpl::wire::GpioImplSetAltFunctionRequest* request, fdf::Arena& arena,
    SetAltFunctionCompleter::Sync& completer) {
  if (request->function > kAltFnMax) {
    zxlogf(ERROR, "AmlGpio::GpioImplSetAltFunction: pin mux alt config out of range %lu",
           request->function);
    return completer.buffer(arena).ReplyError(ZX_ERR_OUT_OF_RANGE);
  }

  zx_status_t status;

  const AmlGpioBlock* block;
  uint32_t pinindex;
  if ((status = AmlPinToBlock(request->index, &block, &pinindex)) != ZX_OK) {
    zxlogf(ERROR, "AmlGpio::GpioImplSetAltFunction: pin not found %u", request->index);
    return completer.buffer(arena).ReplyError(status);
  }

  // Sanity Check: pin_to_block must return a block that contains `pin`
  //               therefore `pin` must be greater than or equal to the first
  //               pin of the block.
  ZX_DEBUG_ASSERT(request->index >= block->start_pin);

  // Each Pin Mux is controlled by a 4 bit wide field in `reg`
  // Compute the offset for this pin.
  uint32_t pin_shift = (request->index - block->start_pin) * 4;
  pin_shift += block->output_shift;
  const uint32_t mux_mask = ~(0x0F << pin_shift);
  const auto fn_val = static_cast<uint32_t>(request->function << pin_shift);

  uint32_t regval = mmios_[block->mmio_index].Read32(block->mux_offset * sizeof(uint32_t));
  regval &= mux_mask;  // Remove the previous value for the mux
  regval |= fn_val;    // Assign the new value to the mux
  mmios_[block->mmio_index].Write32(regval, block->mux_offset * sizeof(uint32_t));

  completer.buffer(arena).ReplySuccess();
}

void AmlGpio::Read(fuchsia_hardware_gpioimpl::wire::GpioImplReadRequest* request, fdf::Arena& arena,
                   ReadCompleter::Sync& completer) {
  zx_status_t status;

  const AmlGpioBlock* block;
  uint32_t pinindex;
  if ((status = AmlPinToBlock(request->index, &block, &pinindex)) != ZX_OK) {
    zxlogf(ERROR, "AmlGpio::GpioImplRead: pin not found %u", request->index);
    return completer.buffer(arena).ReplyError(status);
  }

  uint32_t regval = mmios_[block->mmio_index].Read32(block->input_offset * sizeof(uint32_t));

  const uint32_t readmask = 1 << pinindex;
  completer.buffer(arena).ReplySuccess(regval & readmask ? 1 : 0);
}

void AmlGpio::Write(fuchsia_hardware_gpioimpl::wire::GpioImplWriteRequest* request,
                    fdf::Arena& arena, WriteCompleter::Sync& completer) {
  zx_status_t status;

  const AmlGpioBlock* block;
  uint32_t pinindex;
  if ((status = AmlPinToBlock(request->index, &block, &pinindex)) != ZX_OK) {
    zxlogf(ERROR, "AmlGpio::GpioImplWrite: pin not found %u", request->index);
    return completer.buffer(arena).ReplyError(status);
  }

  uint32_t regval = mmios_[block->mmio_index].Read32(block->output_offset * sizeof(uint32_t));
  if (request->value) {
    regval |= 1 << pinindex;
  } else {
    regval &= ~(1 << pinindex);
  }
  mmios_[block->mmio_index].Write32(regval, block->output_offset * sizeof(uint32_t));

  completer.buffer(arena).ReplySuccess();
}

void AmlGpio::GetInterrupt(fuchsia_hardware_gpioimpl::wire::GpioImplGetInterruptRequest* request,
                           fdf::Arena& arena, GetInterruptCompleter::Sync& completer) {
  zx_status_t status = ZX_OK;

  if (request->index > kMaxGpioIndex) {
    return completer.buffer(arena).ReplyError(ZX_ERR_INVALID_ARGS);
  }

  uint32_t index = GetUnusedIrqIndex(irq_status_);
  if (index > info_.irq_count) {
    zxlogf(ERROR, "No free IRQ indicies %u, irq_count = %u", (int)index, (int)info_.irq_count);
    return completer.buffer(arena).ReplyError(ZX_ERR_NO_RESOURCES);
  }

  for (uint32_t i = 0; i < info_.irq_count; i++) {
    if (irq_info_[i] == request->index) {
      zxlogf(ERROR, "GPIO Interrupt already configured for this pin %u", (int)index);
      return completer.buffer(arena).ReplyError(ZX_ERR_ALREADY_EXISTS);
    }
  }
  zxlogf(DEBUG, "GPIO Interrupt index %d allocated", (int)index);
  const AmlGpioBlock* block;
  uint32_t pinindex;
  if ((status = AmlPinToBlock(request->index, &block, &pinindex)) != ZX_OK) {
    zxlogf(ERROR, "AmlGpio::GpioImplGetInterrupt: pin not found %u", request->index);
    return completer.buffer(arena).ReplyError(status);
  }
  uint32_t flags_ = request->flags;
  if (request->flags == ZX_INTERRUPT_MODE_EDGE_LOW) {
    // GPIO controller sets the polarity
    flags_ = ZX_INTERRUPT_MODE_EDGE_HIGH;
  } else if (request->flags == ZX_INTERRUPT_MODE_LEVEL_LOW) {
    flags_ = ZX_INTERRUPT_MODE_LEVEL_HIGH;
  }

  // Configure GPIO Interrupt EDGE and Polarity
  uint32_t mode_reg_val =
      mmio_interrupt_.Read32(gpio_interrupt_->edge_polarity_offset * sizeof(uint32_t));

  switch (request->flags & ZX_INTERRUPT_MODE_MASK) {
    case ZX_INTERRUPT_MODE_EDGE_LOW:
      mode_reg_val |= (1 << index);
      mode_reg_val |= ((1 << index) << kGpioInterruptPolarityShift);
      break;
    case ZX_INTERRUPT_MODE_EDGE_HIGH:
      mode_reg_val |= (1 << index);
      mode_reg_val &= ~((1 << index) << kGpioInterruptPolarityShift);
      break;
    case ZX_INTERRUPT_MODE_LEVEL_LOW:
      mode_reg_val &= ~(1 << index);
      mode_reg_val |= ((1 << index) << kGpioInterruptPolarityShift);
      break;
    case ZX_INTERRUPT_MODE_LEVEL_HIGH:
      mode_reg_val &= ~(1 << index);
      mode_reg_val &= ~((1 << index) << kGpioInterruptPolarityShift);
      break;
    default:
      return completer.buffer(arena).ReplyError(ZX_ERR_INVALID_ARGS);
  }
  mmio_interrupt_.Write32(mode_reg_val, gpio_interrupt_->edge_polarity_offset * sizeof(uint32_t));

  // Configure Interrupt Select Filter
  mmio_interrupt_.SetBits32(0x7 << (index * kBitsPerFilterSelect),
                            gpio_interrupt_->filter_select_offset * sizeof(uint32_t));

  // Configure GPIO interrupt
  const uint32_t pin_select_bit = index * kBitsPerGpioInterrupt;
  const uint32_t pin_select_offset = gpio_interrupt_->pin_select_offset + (pin_select_bit / 32);
  const uint32_t pin_select_index = pin_select_bit % 32;
  // Select GPIO IRQ(index) and program it to the requested GPIO PIN
  mmio_interrupt_.ModifyBits32((request->index - block->pin_block) + block->pin_start,
                               pin_select_index, kBitsPerGpioInterrupt,
                               pin_select_offset * sizeof(uint32_t));

  // Create Interrupt Object
  zx::interrupt out_irq;
  if ((status = pdev_.GetInterrupt(index, flags_, &out_irq)) != ZX_OK) {
    zxlogf(ERROR, "AmlGpio::GpioImplGetInterrupt: pdev_get_interrupt failed %d", status);
    return completer.buffer(arena).ReplyError(status);
  }

  irq_status_ |= static_cast<uint8_t>(1 << index);
  irq_info_[index] = static_cast<uint16_t>(request->index);

  completer.buffer(arena).ReplySuccess(std::move(out_irq));
}

void AmlGpio::ReleaseInterrupt(
    fuchsia_hardware_gpioimpl::wire::GpioImplReleaseInterruptRequest* request, fdf::Arena& arena,
    ReleaseInterruptCompleter::Sync& completer) {
  for (uint32_t i = 0; i < info_.irq_count; i++) {
    if (irq_info_[i] == request->index) {
      irq_status_ &= static_cast<uint8_t>(~(1 << i));
      irq_info_[i] = kMaxGpioIndex + 1;
      return completer.buffer(arena).ReplySuccess();
    }
  }
  return completer.buffer(arena).ReplyError(ZX_ERR_NOT_FOUND);
}

void AmlGpio::SetPolarity(fuchsia_hardware_gpioimpl::wire::GpioImplSetPolarityRequest* request,
                          fdf::Arena& arena, SetPolarityCompleter::Sync& completer) {
  int irq_index = -1;
  if (request->index > kMaxGpioIndex) {
    return completer.buffer(arena).ReplyError(ZX_ERR_INVALID_ARGS);
  }

  for (uint32_t i = 0; i < info_.irq_count; i++) {
    if (irq_info_[i] == request->index) {
      irq_index = i;
      break;
    }
  }
  if (irq_index == -1) {
    return completer.buffer(arena).ReplyError(ZX_ERR_NOT_FOUND);
  }

  // Configure GPIO Interrupt EDGE and Polarity
  if (request->polarity == fuchsia_hardware_gpio::GpioPolarity::kHigh) {
    mmio_interrupt_.ClearBits32(((1 << irq_index) << kGpioInterruptPolarityShift),
                                gpio_interrupt_->edge_polarity_offset * sizeof(uint32_t));
  } else {
    mmio_interrupt_.SetBits32(((1 << irq_index) << kGpioInterruptPolarityShift),
                              gpio_interrupt_->edge_polarity_offset * sizeof(uint32_t));
  }
  completer.buffer(arena).ReplySuccess();
}

void AmlGpio::GetDriveStrength(
    fuchsia_hardware_gpioimpl::wire::GpioImplGetDriveStrengthRequest* request, fdf::Arena& arena,
    GetDriveStrengthCompleter::Sync& completer) {
  zx_status_t st;

  if (info_.pid == PDEV_PID_AMLOGIC_A113) {
    return completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

  const AmlGpioBlock* block;
  uint32_t pinindex;

  if ((st = AmlPinToBlock(request->index, &block, &pinindex)) != ZX_OK) {
    zxlogf(ERROR, "AmlGpio::GpioImplGetDriveStrength: pin not found %u", request->index);
    return completer.buffer(arena).ReplyError(st);
  }

  pinindex = request->index - block->pin_block;
  if (pinindex >= kMaxPinsInDSReg) {
    pinindex = pinindex % kMaxPinsInDSReg;
  }

  const uint32_t shift = pinindex * 2;

  uint32_t regval = mmios_[block->mmio_index].Read32(block->ds_offset * sizeof(uint32_t));
  uint32_t value = (regval >> shift) & 0x3;

  uint64_t out{};
  switch (value) {
    case DRV_500UA:
      out = 500;
      break;
    case DRV_2500UA:
      out = 2500;
      break;
    case DRV_3000UA:
      out = 3000;
      break;
    case DRV_4000UA:
      out = 4000;
      break;
    default:
      zxlogf(ERROR, "AmlGpio::GpioImplGetDriveStrength: Unexpected drive strength value: %u",
             value);
      return completer.buffer(arena).ReplyError(ZX_ERR_INTERNAL);
  }

  completer.buffer(arena).ReplySuccess(out);
}

void AmlGpio::SetDriveStrength(
    fuchsia_hardware_gpioimpl::wire::GpioImplSetDriveStrengthRequest* request, fdf::Arena& arena,
    SetDriveStrengthCompleter::Sync& completer) {
  zx_status_t status;

  if (info_.pid == PDEV_PID_AMLOGIC_A113) {
    return completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

  const AmlGpioBlock* block;
  uint32_t pinindex;
  if ((status = AmlPinToBlock(request->index, &block, &pinindex)) != ZX_OK) {
    zxlogf(ERROR, "AmlGpio::GpioImplSetDriveStrength: pin not found %u", request->index);
    return completer.buffer(arena).ReplyError(status);
  }

  DriveStrength ds_val = DRV_4000UA;
  uint64_t ua = request->ds_ua;
  if (ua <= 500) {
    ds_val = DRV_500UA;
    ua = 500;
  } else if (ua <= 2500) {
    ds_val = DRV_2500UA;
    ua = 2500;
  } else if (ua <= 3000) {
    ds_val = DRV_3000UA;
    ua = 3000;
  } else if (ua <= 4000) {
    ds_val = DRV_4000UA;
    ua = 4000;
  } else {
    zxlogf(ERROR,
           "AmlGpio::GpioImplSetDriveStrength: invalid drive strength %lu, default to 4000 uA\n",
           ua);
    ds_val = DRV_4000UA;
    ua = 4000;
  }

  pinindex = request->index - block->pin_block;
  if (pinindex >= kMaxPinsInDSReg) {
    pinindex = pinindex % kMaxPinsInDSReg;
  }

  // 2 bits for each pin
  const uint32_t shift = pinindex * 2;
  const uint32_t mask = ~(0x3 << shift);
  uint32_t regval = mmios_[block->mmio_index].Read32(block->ds_offset * sizeof(uint32_t));
  regval = (regval & mask) | (ds_val << shift);
  mmios_[block->mmio_index].Write32(regval, block->ds_offset * sizeof(uint32_t));

  completer.buffer(arena).ReplySuccess(ua);
}

void AmlGpio::GetPins(fdf::Arena& arena, GetPinsCompleter::Sync& completer) {}

void AmlGpio::GetInitSteps(fdf::Arena& arena, GetInitStepsCompleter::Sync& completer) {}

void AmlGpio::GetControllerId(fdf::Arena& arena, GetControllerIdCompleter::Sync& completer) {
  completer.buffer(arena).Reply(0);
}

static constexpr zx_driver_ops_t driver_ops = []() {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = AmlGpio::Create;
  return ops;
}();

}  // namespace gpio

ZIRCON_DRIVER(aml_gpio, gpio::driver_ops, "zircon", "0.1");
