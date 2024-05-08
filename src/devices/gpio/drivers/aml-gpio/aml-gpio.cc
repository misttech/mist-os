// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "aml-gpio.h"

#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/fpromise/bridge.h>

#include <algorithm>
#include <cstdint>
#include <memory>

#include <bind/fuchsia/hardware/gpioimpl/cpp/bind.h>
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
  MMIO_COUNT,
};

void AmlGpioDriver::Start(fdf::StartCompleter completer) {
  parent_.Bind(std::move(node()), dispatcher());

  compat_server_.Begin(
      incoming(), outgoing(), node_name(), name(),
      [this, completer = std::move(completer)](zx::result<> result) mutable {
        if (result.is_error()) {
          FDF_LOG(ERROR, "Failed to initialize compat server: %s", result.status_string());
          return completer(result.take_error());
        }

        OnCompatServerInitialized(std::move(completer));
      },
      compat::ForwardMetadata::Some({DEVICE_METADATA_GPIO_PINS, DEVICE_METADATA_GPIO_CONTROLLER,
                                     DEVICE_METADATA_GPIO_INIT,
                                     DEVICE_METADATA_SCHEDULER_ROLE_NAME}));
}

void AmlGpioDriver::OnCompatServerInitialized(fdf::StartCompleter completer) {
  zx::result pdev_client = incoming()->Connect<fuchsia_hardware_platform_device::Service::Device>();
  if (pdev_client.is_error()) {
    FDF_LOG(ERROR, "Failed to connect to platform device: %s", pdev_client.status_string());
    return completer(pdev_client.take_error());
  }

  pdev_.Bind(*std::move(pdev_client), dispatcher());

  pdev_->GetNodeDeviceInfo().Then([this, completer = std::move(completer)](auto& info) mutable {
    if (!info.ok()) {
      FDF_LOG(ERROR, "Call to get device info failed: %s", info.status_string());
      return completer(zx::error(info.status()));
    }
    if (info->is_error()) {
      FDF_LOG(ERROR, "Failed to get device info: %s", zx_status_get_string(info.status()));
      return completer(info->take_error());
    }
    if (!info->value()->has_pid() || !info->value()->has_irq_count()) {
      FDF_LOG(ERROR, "No pid or irq_count entry in device info");
      return completer(zx::error(ZX_ERR_BAD_STATE));
    }

    OnGetNodeDeviceInfo(*info->value(), std::move(completer));
  });
}

void AmlGpioDriver::OnGetNodeDeviceInfo(
    const fuchsia_hardware_platform_device::wire::NodeDeviceInfo& info,
    fdf::StartCompleter completer) {
  if (info.pid() == 0) {
    // TODO(https://fxbug.dev/318736574) : Remove and rely only on GetDeviceInfo.
    pdev_->GetBoardInfo().Then([this, irq_count = info.irq_count(),
                                completer = std::move(completer)](auto& board_info) mutable {
      if (!board_info.ok()) {
        FDF_LOG(ERROR, "Call to get board info failed: %s", board_info.status_string());
        return completer(zx::error(board_info.status()));
      }
      if (board_info->is_error()) {
        FDF_LOG(ERROR, "Failed to get board info: %s", zx_status_get_string(board_info.status()));
        return completer(board_info->take_error());
      }
      if (!board_info->value()->has_vid() || !board_info->value()->has_pid()) {
        FDF_LOG(ERROR, "No vid or pid entry in board info");
        return completer(zx::error(ZX_ERR_BAD_STATE));
      }

      OnGetBoardInfo(*board_info->value(), irq_count, std::move(completer));
    });
  } else {
    MapMmios(info.pid(), info.irq_count(), std::move(completer));
  }
}

void AmlGpioDriver::OnGetBoardInfo(
    const fuchsia_hardware_platform_device::wire::BoardInfo& board_info, uint32_t irq_count,
    fdf::StartCompleter completer) {
  uint32_t pid = 0;
  if (board_info.vid() == PDEV_VID_AMLOGIC) {
    pid = board_info.pid();
  } else if (board_info.vid() == PDEV_VID_GOOGLE) {
    switch (board_info.pid()) {
      case PDEV_PID_ASTRO:
        pid = PDEV_PID_AMLOGIC_S905D2;
        break;
      case PDEV_PID_SHERLOCK:
        pid = PDEV_PID_AMLOGIC_T931;
        break;
      case PDEV_PID_NELSON:
        pid = PDEV_PID_AMLOGIC_S905D3;
        break;
      default:
        FDF_LOG(ERROR, "Unsupported PID 0x%x for VID 0x%x", board_info.pid(), board_info.vid());
        return completer(zx::error(ZX_ERR_INVALID_ARGS));
    }
  } else if (board_info.vid() == PDEV_VID_KHADAS) {
    switch (board_info.pid()) {
      case PDEV_PID_VIM3:
        pid = PDEV_PID_AMLOGIC_A311D;
        break;
      default:
        FDF_LOG(ERROR, "Unsupported PID 0x%x for VID 0x%x", board_info.pid(), board_info.vid());
        return completer(zx::error(ZX_ERR_INVALID_ARGS));
    }
  } else {
    FDF_LOG(ERROR, "Unsupported VID 0x%x", board_info.vid());
    return completer(zx::error(ZX_ERR_INVALID_ARGS));
  }

  MapMmios(pid, irq_count, std::move(completer));
}

void AmlGpioDriver::MapMmios(uint32_t pid, uint32_t irq_count, fdf::StartCompleter completer) {
  constexpr int kMmioIds[] = {MMIO_GPIO, MMIO_GPIO_AO, MMIO_GPIO_INTERRUPTS};
  static_assert(std::size(kMmioIds) == MMIO_COUNT);

  std::vector<fpromise::promise<fdf::MmioBuffer, zx_status_t>> promises;
  for (const int mmio_id : kMmioIds) {
    promises.push_back(MapMmio(pdev_, mmio_id));
  }

  auto task =
      fpromise::join_promise_vector(std::move(promises))
          .then([this, pid, irq_count, completer = std::move(completer)](
                    fpromise::result<std::vector<fpromise::result<fdf::MmioBuffer, zx_status_t>>>&
                        results) mutable {
            ZX_DEBUG_ASSERT(results.is_ok());

            std::vector<fdf::MmioBuffer> mmios;
            for (auto& result : results.value()) {
              if (result.is_error()) {
                return completer(zx::error(result.error()));
              }
              mmios.push_back(std::move(result.value()));
            }

            AddNode(pid, irq_count, std::move(mmios), std::move(completer));
          });
  executor_.schedule_task(std::move(task));
}

void AmlGpioDriver::AddNode(uint32_t pid, uint32_t irq_count, std::vector<fdf::MmioBuffer> mmios,
                            fdf::StartCompleter completer) {
  ZX_DEBUG_ASSERT(mmios.size() == MMIO_COUNT);

  cpp20::span<const AmlGpioBlock> gpio_blocks;
  const AmlGpioInterrupt* gpio_interrupt;

  switch (pid) {
    case PDEV_PID_AMLOGIC_A113:
      gpio_blocks = {a113_gpio_blocks, std::size(a113_gpio_blocks)};
      gpio_interrupt = &a113_interrupt_block;
      break;
    case PDEV_PID_AMLOGIC_S905D2:
    case PDEV_PID_AMLOGIC_T931:
    case PDEV_PID_AMLOGIC_A311D:
    case PDEV_PID_AMLOGIC_S905D3:
      // S905D2, T931, A311D, S905D3 are identical.
      gpio_blocks = {s905d2_gpio_blocks, std::size(s905d2_gpio_blocks)};
      gpio_interrupt = &s905d2_interrupt_block;
      break;
    case PDEV_PID_AMLOGIC_A5:
      gpio_blocks = {a5_gpio_blocks, std::size(a5_gpio_blocks)};
      gpio_interrupt = &a5_interrupt_block;
      break;
    case PDEV_PID_AMLOGIC_A1:
      gpio_blocks = {a1_gpio_blocks, std::size(a1_gpio_blocks)};
      gpio_interrupt = &a1_interrupt_block;
      break;
    default:
      FDF_LOG(ERROR, "Unsupported SOC PID %u", pid);
      return completer(zx::error(ZX_ERR_INVALID_ARGS));
  }

  fbl::AllocChecker ac;

  fbl::Array<uint16_t> irq_info(new (&ac) uint16_t[irq_count], irq_count);
  if (!ac.check()) {
    FDF_LOG(ERROR, "irq_info alloc failed");
    return completer(zx::error(ZX_ERR_NO_MEMORY));
  }
  std::fill(irq_info.begin(), irq_info.end(), kMaxGpioIndex + 1);  // initialize irq_info

  zx::result pdev_client = incoming()->Connect<fuchsia_hardware_platform_device::Service::Device>();
  if (pdev_client.is_error()) {
    FDF_LOG(ERROR, "Failed to connect to platform device: %s", pdev_client.status_string());
    return completer(pdev_client.take_error());
  }

  device_.reset(new (&ac)
                    AmlGpio(*std::move(pdev_client), std::move(mmios[MMIO_GPIO]),
                            std::move(mmios[MMIO_GPIO_AO]), std::move(mmios[MMIO_GPIO_INTERRUPTS]),
                            gpio_blocks, gpio_interrupt, pid, std::move(irq_info)));
  if (!ac.check()) {
    FDF_LOG(ERROR, "Device object alloc failed");
    return completer(zx::error(ZX_ERR_NO_MEMORY));
  }

  {
    fuchsia_hardware_gpioimpl::Service::InstanceHandler handler({
        .device = device_->CreateHandler(),
    });
    auto result = outgoing()->AddService<fuchsia_hardware_gpioimpl::Service>(std::move(handler));
    if (result.is_error()) {
      FDF_LOG(ERROR, "AddService failed: %s", result.status_string());
      return completer(zx::error(result.error_value()));
    }
  }

  zx::result controller_endpoints =
      fidl::CreateEndpoints<fuchsia_driver_framework::NodeController>();
  if (!controller_endpoints.is_ok()) {
    FDF_LOG(ERROR, "Failed to create controller endpoints: %s",
            controller_endpoints.status_string());
    return completer(controller_endpoints.take_error());
  }

  controller_.Bind(std::move(controller_endpoints->client), dispatcher());

  fidl::Arena arena;

  fidl::VectorView<fuchsia_driver_framework::wire::NodeProperty> properties(arena, 1);
  properties[0] = fdf::MakeProperty(arena, bind_fuchsia_hardware_gpioimpl::SERVICE,
                                    bind_fuchsia_hardware_gpioimpl::SERVICE_DRIVERTRANSPORT);

  std::vector offers = compat_server_.CreateOffers2(arena);
  offers.push_back(
      fdf::MakeOffer2<fuchsia_hardware_gpioimpl::Service>(arena, component::kDefaultInstance));

  const auto args = fuchsia_driver_framework::wire::NodeAddArgs::Builder(arena)
                        .name(arena, name())
                        .offers2(arena, std::move(offers))
                        .properties(properties)
                        .Build();

  parent_->AddChild(args, std::move(controller_endpoints->server), {})
      .Then([completer = std::move(completer)](auto& result) mutable {
        if (!result.ok()) {
          FDF_LOG(ERROR, "Call to add child failed: %s", result.status_string());
          return completer(zx::error(result.status()));
        }
        if (result->is_error()) {
          FDF_LOG(ERROR, "Failed to add child");
          return completer(zx::error(ZX_ERR_INTERNAL));
        }

        completer(zx::ok());
      });
}

zx_status_t AmlGpio::AmlPinToBlock(const uint32_t pin, const AmlGpioBlock** out_block,
                                   uint32_t* out_pin_index) const {
  ZX_DEBUG_ASSERT(out_block && out_pin_index);

  for (const AmlGpioBlock& gpio_block : gpio_blocks_) {
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
    FDF_LOG(ERROR, "Pin not found %u", request->index);
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
    FDF_LOG(ERROR, "Pin not found %u", request->index);
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
    FDF_LOG(ERROR, "Pin mux alt config out of range %lu", request->function);
    return completer.buffer(arena).ReplyError(ZX_ERR_OUT_OF_RANGE);
  }

  zx_status_t status;

  const AmlGpioBlock* block;
  uint32_t pinindex;
  if ((status = AmlPinToBlock(request->index, &block, &pinindex)) != ZX_OK) {
    FDF_LOG(ERROR, "Pin not found %u", request->index);
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
    FDF_LOG(ERROR, "Pin not found %u", request->index);
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
    FDF_LOG(ERROR, "Pin not found %u", request->index);
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
  if (index > irq_info_.size()) {
    FDF_LOG(ERROR, "No free IRQ indicies %u, irq_count = %zu", (int)index, irq_info_.size());
    return completer.buffer(arena).ReplyError(ZX_ERR_NO_RESOURCES);
  }

  for (uint16_t irq : irq_info_) {
    if (irq == request->index) {
      FDF_LOG(ERROR, "GPIO Interrupt already configured for this pin %u", (int)index);
      return completer.buffer(arena).ReplyError(ZX_ERR_ALREADY_EXISTS);
    }
  }
  FDF_LOG(DEBUG, "GPIO Interrupt index %d allocated", (int)index);
  const AmlGpioBlock* block;
  uint32_t pinindex;
  if ((status = AmlPinToBlock(request->index, &block, &pinindex)) != ZX_OK) {
    FDF_LOG(ERROR, "Pin not found %u", request->index);
    return completer.buffer(arena).ReplyError(status);
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

  // Hold this IRQ index while the GetInterrupt call is pending.
  irq_status_ |= static_cast<uint8_t>(1 << index);
  irq_info_[index] = static_cast<uint16_t>(request->index);

  // Create Interrupt Object, removing the requested polarity, since the polarity is managed
  // by the GPIO HW via MMIO setting above.
  // Interrupt modes are represented by values (not a bitmask) within the ZX_INTERRUPT_MODE_MASK.
  // We only change ZX_INTERRUPT_MODE_[EDGE|LEVEL]_LOW values, passing other values in flags
  // unchanged including values outside ZX_INTERRUPT_MODE_MASK.
  uint32_t mode = request->flags & ZX_INTERRUPT_MODE_MASK;
  uint32_t flags = request->flags & ~ZX_INTERRUPT_MODE_MASK;
  if (mode == ZX_INTERRUPT_MODE_EDGE_LOW) {
    mode = ZX_INTERRUPT_MODE_EDGE_HIGH;
  } else if (mode == ZX_INTERRUPT_MODE_LEVEL_LOW) {
    mode = ZX_INTERRUPT_MODE_LEVEL_HIGH;
  }
  flags |= mode;
  pdev_->GetInterruptById(index, flags)
      .Then([this, index, irq_index = request->index,
             completer = completer.ToAsync()](auto& out_irq) mutable {
        fdf::Arena arena('GPIO');
        // ReleaseInterrupt was called before we got the interrupt from platform bus.
        if (irq_info_[index] != irq_index) {
          FDF_LOG(WARNING, "Pin %u interrupt released before it could be returned to the client",
                  irq_index);
          return completer.buffer(arena).ReplyError(ZX_ERR_CANCELED);
        }

        // The call failed, release this IRQ index.
        if (!out_irq.ok() || out_irq->is_error()) {
          irq_status_ &= static_cast<uint8_t>(~(1 << index));
          irq_info_[index] = kMaxGpioIndex + 1;
        }

        if (!out_irq.ok()) {
          FDF_LOG(ERROR, "Call to pdev_get_interrupt failed: %s", out_irq.status_string());
          return completer.buffer(arena).ReplyError(out_irq.status());
        }
        if (out_irq->is_error()) {
          FDF_LOG(ERROR, "pdev_get_interrupt failed: %s", out_irq.status_string());
          return completer.buffer(arena).Reply(out_irq->take_error());
        }

        completer.buffer(arena).ReplySuccess(std::move(out_irq->value()->irq));
      });
}

void AmlGpio::ReleaseInterrupt(
    fuchsia_hardware_gpioimpl::wire::GpioImplReleaseInterruptRequest* request, fdf::Arena& arena,
    ReleaseInterruptCompleter::Sync& completer) {
  for (uint32_t i = 0; i < irq_info_.size(); i++) {
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

  for (uint32_t i = 0; i < irq_info_.size(); i++) {
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

  if (pid_ == PDEV_PID_AMLOGIC_A113) {
    return completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

  const AmlGpioBlock* block;
  uint32_t pinindex;

  if ((st = AmlPinToBlock(request->index, &block, &pinindex)) != ZX_OK) {
    FDF_LOG(ERROR, "Pin not found %u", request->index);
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
      FDF_LOG(ERROR, "Unexpected drive strength value: %u", value);
      return completer.buffer(arena).ReplyError(ZX_ERR_INTERNAL);
  }

  completer.buffer(arena).ReplySuccess(out);
}

void AmlGpio::SetDriveStrength(
    fuchsia_hardware_gpioimpl::wire::GpioImplSetDriveStrengthRequest* request, fdf::Arena& arena,
    SetDriveStrengthCompleter::Sync& completer) {
  zx_status_t status;

  if (pid_ == PDEV_PID_AMLOGIC_A113) {
    return completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

  const AmlGpioBlock* block;
  uint32_t pinindex;
  if ((status = AmlPinToBlock(request->index, &block, &pinindex)) != ZX_OK) {
    FDF_LOG(ERROR, "Pin not found %u", request->index);
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
    FDF_LOG(ERROR, "Invalid drive strength %lu, default to 4000 uA", ua);
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

fpromise::promise<fdf::MmioBuffer, zx_status_t> AmlGpioDriver::MapMmio(
    fidl::WireClient<fuchsia_hardware_platform_device::Device>& pdev, uint32_t mmio_id) {
  fpromise::bridge<fdf::MmioBuffer, zx_status_t> bridge;

  pdev->GetMmioById(mmio_id).Then(
      [mmio_id, completer = std::move(bridge.completer)](auto& result) mutable {
        if (!result.ok()) {
          FDF_LOG(ERROR, "Call to get MMIO %u failed: %s", mmio_id, result.status_string());
          return completer.complete_error(result.status());
        }
        if (result->is_error()) {
          FDF_LOG(ERROR, "Failed to get MMIO %u: %s", mmio_id,
                  zx_status_get_string(result->error_value()));
          return completer.complete_error(result->error_value());
        }

        auto& mmio = *result->value();
        if (!mmio.has_offset() || !mmio.has_size() || !mmio.has_vmo()) {
          FDF_LOG(ERROR, "Invalid MMIO returned for ID %u", mmio_id);
          return completer.complete_error(ZX_ERR_BAD_STATE);
        }

        zx::result mmio_buffer = fdf::MmioBuffer::Create(
            mmio.offset(), mmio.size(), std::move(mmio.vmo()), ZX_CACHE_POLICY_UNCACHED_DEVICE);
        if (mmio_buffer.is_error()) {
          FDF_LOG(ERROR, "Failed to map MMIO %u: %s", mmio_id,
                  zx_status_get_string(mmio_buffer.error_value()));
          return completer.complete_error(mmio_buffer.error_value());
        }

        completer.complete_ok(*std::move(mmio_buffer));
      });

  return bridge.consumer.promise_or(fpromise::error(ZX_ERR_BAD_STATE));
}

}  // namespace gpio

FUCHSIA_DRIVER_EXPORT(gpio::AmlGpioDriver);
