// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "aml-gpio.h"

#include <lib/ddk/metadata.h>
#include <lib/ddk/platform-defs.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/fpromise/bridge.h>
#include <lib/trace/event.h>

#include <algorithm>
#include <cstdint>

#include <bind/fuchsia/hardware/pinimpl/cpp/bind.h>
#include <fbl/alloc_checker.h>

#include "a1-blocks.h"
#include "a113-blocks.h"
#include "a5-blocks.h"
#include "s905d2-blocks.h"

namespace {

constexpr int kAltFnMax = 15;
constexpr int kMaxPinsInDSReg = 16;
constexpr int kGpioInterruptPolarityShift = 16;
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
      compat::ForwardMetadata::Some(
          {DEVICE_METADATA_GPIO_CONTROLLER, DEVICE_METADATA_SCHEDULER_ROLE_NAME}));
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

  fbl::Array<AmlGpio::InterruptInfo> irq_info(new (&ac) AmlGpio::InterruptInfo[irq_count],
                                              irq_count);
  if (!ac.check()) {
    FDF_LOG(ERROR, "irq_info alloc failed");
    return completer(zx::error(ZX_ERR_NO_MEMORY));
  }

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
    fuchsia_hardware_pinimpl::Service::InstanceHandler handler({
        .device = device_->CreateHandler(),
    });
    auto result = outgoing()->AddService<fuchsia_hardware_pinimpl::Service>(std::move(handler));
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
  properties[0] = fdf::MakeProperty(arena, bind_fuchsia_hardware_pinimpl::SERVICE,
                                    bind_fuchsia_hardware_pinimpl::SERVICE_DRIVERTRANSPORT);

  std::vector offers = compat_server_.CreateOffers2(arena);
  offers.push_back(
      fdf::MakeOffer2<fuchsia_hardware_pinimpl::Service>(arena, component::kDefaultInstance));

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

void AmlGpio::Read(fuchsia_hardware_pinimpl::wire::PinImplReadRequest* request, fdf::Arena& arena,
                   ReadCompleter::Sync& completer) {
  zx_status_t status;

  const AmlGpioBlock* block;
  uint32_t pinindex;
  if ((status = AmlPinToBlock(request->pin, &block, &pinindex)) != ZX_OK) {
    FDF_LOG(ERROR, "Pin not found %u", request->pin);
    return completer.buffer(arena).ReplyError(status);
  }

  uint32_t regval = mmios_[block->mmio_index].Read32(block->input_offset * sizeof(uint32_t));

  const uint32_t readmask = 1 << pinindex;
  completer.buffer(arena).ReplySuccess(regval & readmask ? 1 : 0);
}

void AmlGpio::SetBufferMode(fuchsia_hardware_pinimpl::wire::PinImplSetBufferModeRequest* request,
                            fdf::Arena& arena, SetBufferModeCompleter::Sync& completer) {
  zx_status_t status;

  const AmlGpioBlock* block;
  uint32_t pinindex;
  if ((status = AmlPinToBlock(request->pin, &block, &pinindex)) != ZX_OK) {
    FDF_LOG(ERROR, "Pin not found %u", request->pin);
    return completer.buffer(arena).ReplyError(status);
  }

  const uint32_t pinmask = 1 << pinindex;

  uint32_t oen_regval = mmios_[block->mmio_index].Read32(block->oen_offset * sizeof(uint32_t));

  if (request->mode == fuchsia_hardware_gpio::BufferMode::kInput) {
    oen_regval |= pinmask;
  } else {
    // Set value before configuring for output
    uint32_t regval = mmios_[block->mmio_index].Read32(block->output_offset * sizeof(uint32_t));
    if (request->mode == fuchsia_hardware_gpio::BufferMode::kOutputHigh) {
      regval |= pinmask;
    } else {
      regval &= ~pinmask;
    }
    oen_regval &= ~pinmask;

    TRACE_DURATION(
        "gpio",
        (request->mode == fuchsia_hardware_gpio::BufferMode::kOutputHigh ? "set-high" : "set-low"),
        "pin", request->pin);
    mmios_[block->mmio_index].Write32(regval, block->output_offset * sizeof(uint32_t));
    TRACE_COUNTER("gpio", "output", request->pin, "value",
                  (request->mode == fuchsia_hardware_gpio::BufferMode::kOutputHigh ? 1 : 0));
  }

  {
    TRACE_DURATION(
        "gpio",
        (request->mode == fuchsia_hardware_gpio::BufferMode::kInput ? "set-input" : "set-output"),
        "pin", request->pin);
    mmios_[block->mmio_index].Write32(oen_regval, block->oen_offset * sizeof(uint32_t));
  }

  completer.buffer(arena).ReplySuccess();
}

void AmlGpio::GetInterrupt(fuchsia_hardware_pinimpl::wire::PinImplGetInterruptRequest* request,
                           fdf::Arena& arena, GetInterruptCompleter::Sync& completer) {
  zx_status_t status = ZX_OK;

  if (request->pin > kMaxGpioIndex) {
    return completer.buffer(arena).ReplyError(ZX_ERR_INVALID_ARGS);
  }

  uint32_t index = GetUnusedIrqIndex(irq_status_);
  if (index > irq_info_.size()) {
    FDF_LOG(ERROR, "No free IRQ indicies %u, irq_count = %zu", (int)index, irq_info_.size());
    return completer.buffer(arena).ReplyError(ZX_ERR_NO_RESOURCES);
  }

  for (const InterruptInfo& irq : irq_info_) {
    if (irq.pin == request->pin) {
      FDF_LOG(ERROR, "GPIO Interrupt already configured for this pin %u", (int)index);
      return completer.buffer(arena).ReplyError(ZX_ERR_ALREADY_EXISTS);
    }
  }
  FDF_LOG(DEBUG, "GPIO Interrupt index %d allocated", (int)index);
  const AmlGpioBlock* block;
  uint32_t pinindex;
  if ((status = AmlPinToBlock(request->pin, &block, &pinindex)) != ZX_OK) {
    FDF_LOG(ERROR, "Pin not found %u", request->pin);
    return completer.buffer(arena).ReplyError(status);
  }

  // Create Interrupt Object, removing the requested polarity, since the polarity is managed by
  // ConfigureInterrupt().

  // TODO(361851116): Manually convert options instead of casting.
  uint32_t flags = static_cast<uint32_t>(request->options);
  // Remove some options that are not relevant or will be replaced and preserve other values.
  flags &= ~(ZX_INTERRUPT_MODE_MASK | ZX_INTERRUPT_VIRTUAL);

  if (pin_irq_modes_[request->pin]) {
    SetInterruptMode(index, *pin_irq_modes_[request->pin]);

    switch (*pin_irq_modes_[request->pin]) {
      case fuchsia_hardware_gpio::InterruptMode::kEdgeLow:
      case fuchsia_hardware_gpio::InterruptMode::kEdgeHigh:
        flags |= ZX_INTERRUPT_MODE_EDGE_HIGH;
        break;
      case fuchsia_hardware_gpio::InterruptMode::kLevelLow:
      case fuchsia_hardware_gpio::InterruptMode::kLevelHigh:
        flags |= ZX_INTERRUPT_MODE_LEVEL_HIGH;
        break;
      default:
        ZX_DEBUG_ASSERT(false);  // ConfigureInterrupt() should have validated the interrupt mode.
    }

    pin_irq_modes_[request->pin].reset();
  } else {
    // Don't set the interrupt mode, and instead read the mode register to determine whether the
    // interrupt is edge- or level-triggered.
    const uint32_t mode_reg_val =
        mmio_interrupt_.Read32(gpio_interrupt_->edge_polarity_offset * sizeof(uint32_t));
    flags |=
        mode_reg_val & (1 << index) ? ZX_INTERRUPT_MODE_EDGE_HIGH : ZX_INTERRUPT_MODE_LEVEL_HIGH;
  }

  // Configure Interrupt Select Filter
  mmio_interrupt_.SetBits32(0x7 << (index * kBitsPerFilterSelect),
                            gpio_interrupt_->filter_select_offset * sizeof(uint32_t));

  // Configure GPIO interrupt
  const uint32_t pin_select_bit = index * kBitsPerGpioInterrupt;
  const uint32_t pin_select_offset = gpio_interrupt_->pin_select_offset + (pin_select_bit / 32);
  const uint32_t pin_select_index = pin_select_bit % 32;
  // Select GPIO IRQ(index) and program it to the requested GPIO PIN
  mmio_interrupt_.ModifyBits32((request->pin - block->pin_block) + block->pin_start,
                               pin_select_index, kBitsPerGpioInterrupt,
                               pin_select_offset * sizeof(uint32_t));

  // Hold this IRQ index while the GetInterrupt call is pending.
  irq_status_ |= static_cast<uint8_t>(1 << index);
  irq_info_[index].pin = static_cast<uint16_t>(request->pin);

  pdev_->GetInterruptById(index, flags)
      .Then([this, index, irq_index = request->pin,
             completer = completer.ToAsync()](auto& out_irq) mutable {
        fdf::Arena arena('GPIO');
        // ReleaseInterrupt was called before we got the interrupt from platform bus.
        if (irq_info_[index].pin != irq_index) {
          FDF_LOG(WARNING, "Pin %u interrupt released before it could be returned to the client",
                  irq_index);
          return completer.buffer(arena).ReplyError(ZX_ERR_CANCELED);
        }

        // The call failed, release this IRQ index.
        if (!out_irq.ok() || out_irq->is_error()) {
          irq_status_ &= static_cast<uint8_t>(~(1 << index));
          irq_info_[index] = InterruptInfo{};
        }

        if (!out_irq.ok()) {
          FDF_LOG(ERROR, "Call to pdev_get_interrupt failed: %s", out_irq.status_string());
          return completer.buffer(arena).ReplyError(out_irq.status());
        }
        if (out_irq->is_error()) {
          FDF_LOG(ERROR, "pdev_get_interrupt failed: %s",
                  zx_status_get_string(out_irq->error_value()));
          return completer.buffer(arena).Reply(out_irq->take_error());
        }

        zx_status_t status =
            out_irq->value()->irq.duplicate(ZX_RIGHT_SAME_RIGHTS, &irq_info_[index].interrupt);
        if (status == ZX_OK) {
          completer.buffer(arena).ReplySuccess(std::move(out_irq->value()->irq));
        } else {
          FDF_LOG(ERROR, "Failed to duplicate interrupt handle: %s", zx_status_get_string(status));
          irq_status_ &= static_cast<uint8_t>(~(1 << index));
          irq_info_[index] = InterruptInfo{};
          completer.buffer(arena).ReplyError(status);
        }
      });
}

void AmlGpio::ConfigureInterrupt(
    fuchsia_hardware_pinimpl::wire::PinImplConfigureInterruptRequest* request, fdf::Arena& arena,
    ConfigureInterruptCompleter::Sync& completer) {
  if (request->pin > kMaxGpioIndex) {
    return completer.buffer(arena).ReplyError(ZX_ERR_INVALID_ARGS);
  }
  if (!request->config.has_mode()) {
    return completer.buffer(arena).ReplyError(ZX_ERR_INVALID_ARGS);
  }
  if (request->config.mode() != fuchsia_hardware_gpio::InterruptMode::kEdgeLow &&
      request->config.mode() != fuchsia_hardware_gpio::InterruptMode::kEdgeHigh &&
      request->config.mode() != fuchsia_hardware_gpio::InterruptMode::kLevelLow &&
      request->config.mode() != fuchsia_hardware_gpio::InterruptMode::kLevelHigh) {
    return completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

  // Configure the interrupt for this pin if there is one. If not, the mode is saved and will be
  // applied when an interrupt is created for this pin.
  for (uint32_t i = 0; i < irq_info_.size(); i++) {
    if (irq_info_[i].pin == request->pin) {
      SetInterruptMode(i, request->config.mode());
      return completer.buffer(arena).ReplySuccess();
    }
  }

  pin_irq_modes_[request->pin] = request->config.mode();
  completer.buffer(arena).ReplySuccess();
}

void AmlGpio::ReleaseInterrupt(
    fuchsia_hardware_pinimpl::wire::PinImplReleaseInterruptRequest* request, fdf::Arena& arena,
    ReleaseInterruptCompleter::Sync& completer) {
  for (uint32_t i = 0; i < irq_info_.size(); i++) {
    if (irq_info_[i].pin == request->pin) {
      irq_status_ &= static_cast<uint8_t>(~(1 << i));
      // Destroy the interrupt so that platform-bus will be able to create a new one for this vector
      // the next time we need it.
      irq_info_[i].interrupt.destroy();
      irq_info_[i] = InterruptInfo{};
      return completer.buffer(arena).ReplySuccess();
    }
  }
  return completer.buffer(arena).ReplyError(ZX_ERR_NOT_FOUND);
}

void AmlGpio::Configure(fuchsia_hardware_pinimpl::wire::PinImplConfigureRequest* request,
                        fdf::Arena& arena, ConfigureCompleter::Sync& completer) {
  zx_status_t status;

  const AmlGpioBlock* block;
  uint32_t pinindex;
  if ((status = AmlPinToBlock(request->pin, &block, &pinindex)) != ZX_OK) {
    FDF_LOG(ERROR, "Pin not found %u", request->pin);
    return completer.buffer(arena).ReplyError(status);
  }

  if (request->config.has_function() && request->config.function() > kAltFnMax) {
    FDF_LOG(ERROR, "Pin mux alt config out of range %lu", request->config.function());
    return completer.buffer(arena).ReplyError(ZX_ERR_OUT_OF_RANGE);
  }
  if (request->config.has_drive_strength_ua() && pid_ == PDEV_PID_AMLOGIC_A113) {
    return completer.buffer(arena).ReplyError(ZX_ERR_NOT_SUPPORTED);
  }

  if (request->config.has_pull()) {
    SetPull(block, pinindex, request->config.pull());
  }
  if (request->config.has_function()) {
    SetFunction(request->pin, block, request->config.function());
  }
  if (request->config.has_drive_strength_ua()) {
    SetDriveStrength(request->pin, block, request->config.drive_strength_ua());
  }

  auto new_config = fuchsia_hardware_pin::wire::Configuration::Builder(arena)
                        .pull(GetPull(block, pinindex))
                        .function(GetFunction(request->pin, block));
  if (pid_ != PDEV_PID_AMLOGIC_A113) {
    new_config.drive_strength_ua(GetDriveStrength(request->pin, block));
  }
  completer.buffer(arena).ReplySuccess(new_config.Build());
}

fuchsia_hardware_pin::Pull AmlGpio::GetPull(const AmlGpioBlock* block, uint32_t pinindex) {
  const uint32_t pinmask = 1 << pinindex;

  uint32_t pull_en_reg_val =
      mmios_[block->mmio_index].Read32(block->pull_en_offset * sizeof(uint32_t));
  if (pull_en_reg_val & pinmask) {
    uint32_t pull_reg_val = mmios_[block->mmio_index].Read32(block->pull_offset * sizeof(uint32_t));
    return pull_reg_val & pinmask ? fuchsia_hardware_pin::Pull::kUp
                                  : fuchsia_hardware_pin::Pull::kDown;
  } else {
    return fuchsia_hardware_pin::Pull::kNone;
  }
}

void AmlGpio::SetPull(const AmlGpioBlock* block, uint32_t pinindex,
                      fuchsia_hardware_pin::Pull pull) {
  const uint32_t pinmask = 1 << pinindex;

  // Set the GPIO as pull-up or pull-down
  uint32_t pull_reg_val = mmios_[block->mmio_index].Read32(block->pull_offset * sizeof(uint32_t));
  uint32_t pull_en_reg_val =
      mmios_[block->mmio_index].Read32(block->pull_en_offset * sizeof(uint32_t));
  if (pull == fuchsia_hardware_pin::Pull::kNone) {
    pull_en_reg_val &= ~pinmask;
  } else {
    if (pull == fuchsia_hardware_pin::Pull::kUp) {
      pull_reg_val |= pinmask;
    } else {
      pull_reg_val &= ~pinmask;
    }
    pull_en_reg_val |= pinmask;
  }

  mmios_[block->mmio_index].Write32(pull_reg_val, block->pull_offset * sizeof(uint32_t));
  mmios_[block->mmio_index].Write32(pull_en_reg_val, block->pull_en_offset * sizeof(uint32_t));
}

uint64_t AmlGpio::GetFunction(uint32_t index, const AmlGpioBlock* block) {
  // Validity Check: pin_to_block must return a block that contains `pin`
  //                 therefore `pin` must be greater than or equal to the first
  //                 pin of the block.
  ZX_DEBUG_ASSERT(index >= block->start_pin);

  // Each Pin Mux is controlled by a 4 bit wide field in `reg`
  // Compute the offset for this pin.
  uint32_t pin_shift = (index - block->start_pin) * 4;
  pin_shift += block->output_shift;

  uint32_t regval = mmios_[block->mmio_index].Read32(block->mux_offset * sizeof(uint32_t));
  return (regval >> pin_shift) & 0x0F;
}

// Configure a pin for an alternate function
void AmlGpio::SetFunction(uint32_t index, const AmlGpioBlock* block, uint64_t function) {
  ZX_DEBUG_ASSERT(index >= block->start_pin);

  uint32_t pin_shift = (index - block->start_pin) * 4;
  pin_shift += block->output_shift;
  const uint32_t mux_mask = ~(0x0F << pin_shift);
  const auto fn_val = static_cast<uint32_t>(function << pin_shift);

  uint32_t regval = mmios_[block->mmio_index].Read32(block->mux_offset * sizeof(uint32_t));
  regval &= mux_mask;  // Remove the previous value for the mux
  regval |= fn_val;    // Assign the new value to the mux
  mmios_[block->mmio_index].Write32(regval, block->mux_offset * sizeof(uint32_t));
}

uint64_t AmlGpio::GetDriveStrength(uint32_t index, const AmlGpioBlock* block) {
  uint32_t pinindex = index - block->pin_block;
  if (pinindex >= kMaxPinsInDSReg) {
    pinindex = pinindex % kMaxPinsInDSReg;
  }

  const uint32_t shift = pinindex * 2;

  uint32_t regval = mmios_[block->mmio_index].Read32(block->ds_offset * sizeof(uint32_t));
  uint32_t value = (regval >> shift) & 0x3;

  constexpr uint64_t kDriveStrengthValuesUa[] = {500, 2500, 3000, 4000};
  return kDriveStrengthValuesUa[value];
}

void AmlGpio::SetDriveStrength(uint32_t index, const AmlGpioBlock* block,
                               uint64_t drive_strength_ua) {
  DriveStrength ds_val = DRV_4000UA;
  if (drive_strength_ua <= 500) {
    ds_val = DRV_500UA;
  } else if (drive_strength_ua <= 2500) {
    ds_val = DRV_2500UA;
  } else if (drive_strength_ua <= 3000) {
    ds_val = DRV_3000UA;
  } else if (drive_strength_ua <= 4000) {
    ds_val = DRV_4000UA;
  } else {
    FDF_LOG(ERROR, "Invalid drive strength %lu, default to 4000 uA", drive_strength_ua);
    ds_val = DRV_4000UA;
  }

  uint32_t pinindex = index - block->pin_block;
  if (pinindex >= kMaxPinsInDSReg) {
    pinindex = pinindex % kMaxPinsInDSReg;
  }

  // 2 bits for each pin
  const uint32_t shift = pinindex * 2;
  const uint32_t mask = ~(0x3 << shift);
  uint32_t regval = mmios_[block->mmio_index].Read32(block->ds_offset * sizeof(uint32_t));
  regval = (regval & mask) | (ds_val << shift);
  mmios_[block->mmio_index].Write32(regval, block->ds_offset * sizeof(uint32_t));
}

void AmlGpio::SetInterruptMode(uint32_t irq_index, fuchsia_hardware_gpio::InterruptMode mode) {
  // Configure GPIO Interrupt EDGE and Polarity
  uint32_t mode_reg_val =
      mmio_interrupt_.Read32(gpio_interrupt_->edge_polarity_offset * sizeof(uint32_t));

  switch (mode) {
    case fuchsia_hardware_gpio::InterruptMode::kEdgeLow:
      mode_reg_val |= (1 << irq_index);
      mode_reg_val |= ((1 << irq_index) << kGpioInterruptPolarityShift);
      break;
    case fuchsia_hardware_gpio::InterruptMode::kEdgeHigh:
      mode_reg_val |= (1 << irq_index);
      mode_reg_val &= ~((1 << irq_index) << kGpioInterruptPolarityShift);
      break;
    case fuchsia_hardware_gpio::InterruptMode::kLevelLow:
      mode_reg_val &= ~(1 << irq_index);
      mode_reg_val |= ((1 << irq_index) << kGpioInterruptPolarityShift);
      break;
    case fuchsia_hardware_gpio::InterruptMode::kLevelHigh:
      mode_reg_val &= ~(1 << irq_index);
      mode_reg_val &= ~((1 << irq_index) << kGpioInterruptPolarityShift);
      break;
    default:
      return;
  }
  mmio_interrupt_.Write32(mode_reg_val, gpio_interrupt_->edge_polarity_offset * sizeof(uint32_t));
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
