// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "gpio.h"

#include <fidl/fuchsia.scheduler/cpp/fidl.h>
#include <lib/ddk/metadata.h>
#include <lib/driver/compat/cpp/metadata.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/node/cpp/add_child.h>
#include <lib/fit/defer.h>
#include <zircon/types.h>

#include <memory>

#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/gpio/cpp/bind.h>
#include <fbl/alloc_checker.h>

namespace gpio {

// Helper functions for converting FIDL result types to zx_status_t and back.

template <typename T>
inline zx_status_t FidlStatus(const T& result) {
  if (result.ok()) {
    return result->is_ok() ? ZX_OK : result->error_value();
  }
  return result.status();
}

inline fit::result<zx_status_t> FidlResult(zx_status_t status) {
  if (status == ZX_OK) {
    return fit::success();
  }
  return fit::error(status);
}

fuchsia_hardware_gpio::GpioFlags PullToGpioFlags(fuchsia_hardware_pin::Pull pull) {
  switch (pull) {
    case fuchsia_hardware_pin::Pull::kDown:
      return fuchsia_hardware_gpio::GpioFlags::kPullDown;
    case fuchsia_hardware_pin::Pull::kUp:
      return fuchsia_hardware_gpio::GpioFlags::kPullUp;
    default:
      return fuchsia_hardware_gpio::GpioFlags::kNoPull;
  }
}

fuchsia_hardware_pin::Pull GpioFlagsToPull(fuchsia_hardware_gpio::GpioFlags flags) {
  switch (flags) {
    case fuchsia_hardware_gpio::GpioFlags::kPullDown:
      return fuchsia_hardware_pin::Pull::kDown;
    case fuchsia_hardware_gpio::GpioFlags::kPullUp:
      return fuchsia_hardware_pin::Pull::kUp;
    default:
      return fuchsia_hardware_pin::Pull::kNone;
  }
}

void GpioDevice::GetPin(GetPinCompleter::Sync& completer) { completer.ReplySuccess(pin_); }

void GpioDevice::GetName(GetNameCompleter::Sync& completer) {
  completer.ReplySuccess(fidl::StringView::FromExternal(name_));
}

void GpioDevice::ConfigIn(ConfigInRequestView request, ConfigInCompleter::Sync& completer) {
  fdf::Arena arena('GPIO');

  if (std::holds_alternative<PinClient>(impl_)) {
    // Emulate ConfigIn by first calling Configure() to set the pull-up/-down, then SetBufferMode()
    // to disable output.
    auto config = fuchsia_hardware_pin::wire::Configuration::Builder(arena)
                      .pull(GpioFlagsToPull(request->flags))
                      .Build();
    std::get<PinClient>(impl_)
        .buffer(arena)
        ->Configure(pin_, config)
        // TODO(42082459): Remove this call after converting to the new SetBufferMode method.
        .ThenExactlyOnce([this, completer = completer.ToAsync()](auto& result) mutable {
          if (!result.ok()) {
            return completer.ReplyError(result.status());
          }
          if (result->is_error()) {
            return completer.ReplyError(result->error_value());
          }

          fdf::Arena arena('GPIO');
          std::get<PinClient>(impl_)
              .buffer(arena)
              ->SetBufferMode(pin_, fuchsia_hardware_gpio::BufferMode::kInput)
              .ThenExactlyOnce(
                  fit::inline_callback<void(fdf::WireUnownedResult<
                                            fuchsia_hardware_pinimpl::PinImpl::SetBufferMode>&),
                                       sizeof(ConfigInCompleter::Async)>(
                      [completer = std::move(completer)](auto& result) mutable {
                        if (result.ok()) {
                          completer.Reply(*result);
                        } else {
                          completer.ReplyError(result.status());
                        }
                      }));
        });
    return;
  }

  std::get<GpioClient>(impl_)
      .buffer(arena)
      ->ConfigIn(pin_, request->flags)
      .ThenExactlyOnce(fit::inline_callback<
                       void(fdf::WireUnownedResult<fuchsia_hardware_gpioimpl::GpioImpl::ConfigIn>&),
                       sizeof(ConfigInCompleter::Async)>(
          [completer = completer.ToAsync()](auto& result) mutable {
            if (!result.ok()) {
              completer.ReplyError(result.status());
            } else if (result->is_error()) {
              completer.ReplyError(result->error_value());
            } else {
              completer.ReplySuccess();
            }
          }));
}

void GpioDevice::ConfigOut(ConfigOutRequestView request, ConfigOutCompleter::Sync& completer) {
  fdf::Arena arena('GPIO');

  if (std::holds_alternative<PinClient>(impl_)) {
    std::get<PinClient>(impl_)
        .buffer(arena)
        ->SetBufferMode(pin_, request->initial_value == 0
                                  ? fuchsia_hardware_gpio::BufferMode::kOutputLow
                                  : fuchsia_hardware_gpio::BufferMode::kOutputHigh)
        .ThenExactlyOnce(
            fit::inline_callback<
                void(fdf::WireUnownedResult<fuchsia_hardware_pinimpl::PinImpl::SetBufferMode>&),
                sizeof(ConfigOutCompleter::Async)>(
                [completer = completer.ToAsync()](auto& result) mutable {
                  if (result.ok()) {
                    completer.Reply(*result);
                  } else {
                    completer.ReplyError(result.status());
                  }
                }));
    return;
  }

  std::get<GpioClient>(impl_)
      .buffer(arena)
      ->ConfigOut(pin_, request->initial_value)
      .ThenExactlyOnce(
          fit::inline_callback<
              void(fdf::WireUnownedResult<fuchsia_hardware_gpioimpl::GpioImpl::ConfigOut>&),
              sizeof(ConfigOutCompleter::Async)>(
              [completer = completer.ToAsync()](auto& result) mutable {
                if (!result.ok()) {
                  completer.ReplyError(result.status());
                } else if (result->is_error()) {
                  completer.ReplyError(result->error_value());
                } else {
                  completer.ReplySuccess();
                }
              }));
}

void GpioDevice::Read(ReadCompleter::Sync& completer) {
  fdf::Arena arena('GPIO');

  if (std::holds_alternative<PinClient>(impl_)) {
    std::get<PinClient>(impl_).buffer(arena)->Read(pin_).ThenExactlyOnce(
        fit::inline_callback<void(fdf::WireUnownedResult<fuchsia_hardware_pinimpl::PinImpl::Read>&),
                             sizeof(ReadCompleter::Async)>(
            [completer = completer.ToAsync()](auto& result) mutable {
              if (!result.ok()) {
                completer.ReplyError(result.status());
              } else if (result->is_error()) {
                completer.ReplyError(result->error_value());
              } else {
                completer.ReplySuccess(result->value()->value);
              }
            }));
    return;
  }

  std::get<GpioClient>(impl_).buffer(arena)->Read(pin_).ThenExactlyOnce(
      fit::inline_callback<void(fdf::WireUnownedResult<fuchsia_hardware_gpioimpl::GpioImpl::Read>&),
                           sizeof(ReadCompleter::Async)>(
          [completer = completer.ToAsync()](auto& result) mutable {
            if (!result.ok()) {
              completer.ReplyError(result.status());
            } else if (result->is_error()) {
              completer.ReplyError(result->error_value());
            } else {
              completer.ReplySuccess(result->value()->value);
            }
          }));
}

void GpioDevice::Write(WriteRequestView request, WriteCompleter::Sync& completer) {
  fdf::Arena arena('GPIO');

  if (std::holds_alternative<PinClient>(impl_)) {
    std::get<PinClient>(impl_)
        .buffer(arena)
        ->SetBufferMode(pin_, request->value == 0 ? fuchsia_hardware_gpio::BufferMode::kOutputLow
                                                  : fuchsia_hardware_gpio::BufferMode::kOutputHigh)
        .ThenExactlyOnce(
            fit::inline_callback<
                void(fdf::WireUnownedResult<fuchsia_hardware_pinimpl::PinImpl::SetBufferMode>&),
                sizeof(WriteCompleter::Async)>(
                [completer = completer.ToAsync()](auto& result) mutable {
                  if (result.ok()) {
                    completer.Reply(*result);
                  } else {
                    completer.ReplyError(result.status());
                  }
                }));
    return;
  }

  std::get<GpioClient>(impl_)
      .buffer(arena)
      ->Write(pin_, request->value)
      .ThenExactlyOnce(fit::inline_callback<
                       void(fdf::WireUnownedResult<fuchsia_hardware_gpioimpl::GpioImpl::Write>&),
                       sizeof(WriteCompleter::Async)>(
          [completer = completer.ToAsync()](auto& result) mutable {
            if (!result.ok()) {
              completer.ReplyError(result.status());
            } else if (result->is_error()) {
              completer.ReplyError(result->error_value());
            } else {
              completer.ReplySuccess();
            }
          }));
}

void GpioDevice::SetDriveStrength(SetDriveStrengthRequestView request,
                                  SetDriveStrengthCompleter::Sync& completer) {
  fdf::Arena arena('GPIO');

  if (std::holds_alternative<PinClient>(impl_)) {
    auto config = fuchsia_hardware_pin::wire::Configuration::Builder(arena)
                      .drive_strength_ua(request->ds_ua)
                      .Build();
    std::get<PinClient>(impl_)
        .buffer(arena)
        ->Configure(pin_, config)
        .ThenExactlyOnce(
            fit::inline_callback<
                void(fdf::WireUnownedResult<fuchsia_hardware_pinimpl::PinImpl::Configure>&),
                sizeof(SetDriveStrengthCompleter::Async)>(
                [completer = completer.ToAsync()](auto& result) mutable {
                  if (!result.ok()) {
                    completer.ReplyError(result.status());
                  } else if (result->is_error()) {
                    completer.ReplyError(result->error_value());
                  } else {
                    // drive_strength_ua must have been set if this method returned success.
                    ZX_DEBUG_ASSERT(result->value()->new_config.has_drive_strength_ua());
                    completer.ReplySuccess(result->value()->new_config.drive_strength_ua());
                  }
                }));
    return;
  }

  std::get<GpioClient>(impl_)
      .buffer(arena)
      ->SetDriveStrength(pin_, request->ds_ua)
      .ThenExactlyOnce(
          fit::inline_callback<
              void(fdf::WireUnownedResult<fuchsia_hardware_gpioimpl::GpioImpl::SetDriveStrength>&),
              sizeof(SetDriveStrengthCompleter::Async)>(
              [completer = completer.ToAsync()](auto& result) mutable {
                if (!result.ok()) {
                  completer.ReplyError(result.status());
                } else if (result->is_error()) {
                  completer.ReplyError(result->error_value());
                } else {
                  completer.ReplySuccess(result->value()->actual_ds_ua);
                }
              }));
}

void GpioDevice::GetDriveStrength(GetDriveStrengthCompleter::Sync& completer) {
  fdf::Arena arena('GPIO');

  if (std::holds_alternative<PinClient>(impl_)) {
    auto config = fuchsia_hardware_pin::wire::Configuration::Builder(arena).Build();
    std::get<PinClient>(impl_)
        .buffer(arena)
        ->Configure(pin_, config)
        .ThenExactlyOnce(
            fit::inline_callback<
                void(fdf::WireUnownedResult<fuchsia_hardware_pinimpl::PinImpl::Configure>&),
                sizeof(GetDriveStrengthCompleter::Async)>(
                [completer = completer.ToAsync()](auto& result) mutable {
                  if (!result.ok()) {
                    completer.ReplyError(result.status());
                  } else if (result->is_error()) {
                    completer.ReplyError(result->error_value());
                  } else if (result->value()->new_config.has_drive_strength_ua()) {
                    completer.ReplySuccess(result->value()->new_config.drive_strength_ua());
                  } else {
                    completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
                  }
                }));
    return;
  }

  std::get<GpioClient>(impl_).buffer(arena)->GetDriveStrength(pin_).ThenExactlyOnce(
      fit::inline_callback<
          void(fdf::WireUnownedResult<fuchsia_hardware_gpioimpl::GpioImpl::GetDriveStrength>&),
          sizeof(GetDriveStrengthCompleter::Async)>(
          [completer = completer.ToAsync()](auto& result) mutable {
            if (!result.ok()) {
              completer.ReplyError(result.status());
            } else if (result->is_error()) {
              completer.ReplyError(result->error_value());
            } else {
              completer.ReplySuccess(result->value()->result_ua);
            }
          }));
}

void GpioDevice::GetInterrupt(GetInterruptRequestView request,
                              GetInterruptCompleter::Sync& completer) {
  fdf::Arena arena('GPIO');

  if (std::holds_alternative<PinClient>(impl_)) {
    fuchsia_hardware_gpio::InterruptMode mode{};
    switch (request->flags & ZX_INTERRUPT_MODE_MASK) {
      case ZX_INTERRUPT_MODE_EDGE_LOW:
        mode = fuchsia_hardware_gpio::InterruptMode::kEdgeLow;
        break;
      case ZX_INTERRUPT_MODE_EDGE_HIGH:
        mode = fuchsia_hardware_gpio::InterruptMode::kEdgeHigh;
        break;
      case ZX_INTERRUPT_MODE_EDGE_BOTH:
        mode = fuchsia_hardware_gpio::InterruptMode::kEdgeBoth;
        break;
      case ZX_INTERRUPT_MODE_LEVEL_LOW:
        mode = fuchsia_hardware_gpio::InterruptMode::kLevelLow;
        break;
      case ZX_INTERRUPT_MODE_LEVEL_HIGH:
        mode = fuchsia_hardware_gpio::InterruptMode::kLevelHigh;
        break;
      default:
        return completer.ReplyError(ZX_ERR_INVALID_ARGS);
    }

    // TODO(361851116): Pass options without casting.
    const auto options = static_cast<fuchsia_hardware_gpio::InterruptOptions>(request->flags);

    // Emulate GetInterrupt by first calling ConfigureInterrupt(), then returning the interrupt from
    // GetInterrupt().
    auto config =
        fuchsia_hardware_gpio::wire::InterruptConfiguration::Builder(arena).mode(mode).Build();
    std::get<PinClient>(impl_)
        .buffer(arena)
        ->ConfigureInterrupt(pin_, config)
        // TODO(42082459): Remove this call after converting to the new GetInterrupt method.
        .ThenExactlyOnce([this, options, completer = completer.ToAsync()](auto& result) mutable {
          if (!result.ok()) {
            return completer.ReplyError(result.status());
          }
          if (result->is_error()) {
            return completer.ReplyError(result->error_value());
          }

          fdf::Arena arena('GPIO');
          std::get<PinClient>(impl_)
              .buffer(arena)
              ->GetInterrupt(pin_, options)
              .ThenExactlyOnce(
                  fit::inline_callback<void(fdf::WireUnownedResult<
                                            fuchsia_hardware_pinimpl::PinImpl::GetInterrupt>&),
                                       sizeof(GetInterruptCompleter::Async)>(
                      [completer = std::move(completer)](auto& result) mutable {
                        if (!result.ok()) {
                          completer.ReplyError(result.status());
                        } else if (result->is_error()) {
                          completer.ReplyError(result->error_value());
                        } else {
                          completer.ReplySuccess(std::move(result->value()->interrupt));
                        }
                      }));
        });
    return;
  }

  std::get<GpioClient>(impl_)
      .buffer(arena)
      ->GetInterrupt(pin_, request->flags)
      .ThenExactlyOnce(
          fit::inline_callback<
              void(fdf::WireUnownedResult<fuchsia_hardware_gpioimpl::GpioImpl::GetInterrupt>&),
              sizeof(GetInterruptCompleter::Async)>(
              [completer = completer.ToAsync()](auto& result) mutable {
                if (!result.ok()) {
                  completer.ReplyError(result.status());
                } else if (result->is_error()) {
                  completer.ReplyError(result->error_value());
                } else {
                  completer.ReplySuccess(std::move(result->value()->irq));
                }
              }));
}

void GpioDevice::ConfigureInterrupt(
    fuchsia_hardware_gpio::wire::GpioConfigureInterruptRequest* request,
    ConfigureInterruptCompleter::Sync& completer) {
  fdf::Arena arena('GPIO');

  if (std::holds_alternative<PinClient>(impl_)) {
    std::get<PinClient>(impl_)
        .buffer(arena)
        ->ConfigureInterrupt(pin_, request->config)
        .ThenExactlyOnce(
            fit::inline_callback<void(fdf::WireUnownedResult<
                                      fuchsia_hardware_pinimpl::PinImpl::ConfigureInterrupt>&),
                                 sizeof(ConfigureInterruptCompleter::Async)>(
                [completer = completer.ToAsync()](auto& result) mutable {
                  if (result.ok()) {
                    completer.Reply(*result);
                  } else {
                    completer.ReplyError(result.status());
                  }
                }));
    return;
  }

  if (!request->config.has_mode()) {
    return completer.ReplyError(ZX_ERR_INVALID_ARGS);
  }

  fuchsia_hardware_gpio::GpioPolarity polarity{};
  switch (request->config.mode()) {
    case fuchsia_hardware_gpio::InterruptMode::kEdgeHigh:
    case fuchsia_hardware_gpio::InterruptMode::kLevelHigh:
      polarity = fuchsia_hardware_gpio::GpioPolarity::kHigh;
      break;
    case fuchsia_hardware_gpio::InterruptMode::kEdgeLow:
    case fuchsia_hardware_gpio::InterruptMode::kLevelLow:
      polarity = fuchsia_hardware_gpio::GpioPolarity::kLow;
      break;
    case fuchsia_hardware_gpio::InterruptMode::kEdgeBoth:
    default:
      // TODO(42082459): Remove this case when gpioimpl is converted to pinimpl.
      return completer.ReplySuccess();
  }

  std::get<GpioClient>(impl_)
      .buffer(arena)
      ->SetPolarity(pin_, polarity)
      .ThenExactlyOnce(
          fit::inline_callback<
              void(fdf::WireUnownedResult<fuchsia_hardware_gpioimpl::GpioImpl::SetPolarity>&),
              sizeof(ConfigureInterruptCompleter::Async)>(
              [completer = completer.ToAsync()](auto& result) mutable {
                if (!result.ok()) {
                  completer.ReplyError(result.status());
                } else if (result->is_error()) {
                  completer.ReplyError(result->error_value());
                } else {
                  completer.ReplySuccess();
                }
              }));
}

void GpioDevice::ReleaseInterrupt(ReleaseInterruptCompleter::Sync& completer) {
  fdf::Arena arena('GPIO');

  if (std::holds_alternative<PinClient>(impl_)) {
    std::get<PinClient>(impl_).buffer(arena)->ReleaseInterrupt(pin_).ThenExactlyOnce(
        fit::inline_callback<
            void(fdf::WireUnownedResult<fuchsia_hardware_pinimpl::PinImpl::ReleaseInterrupt>&),
            sizeof(ReleaseInterruptCompleter::Async)>(
            [completer = completer.ToAsync()](auto& result) mutable {
              if (result.ok()) {
                completer.Reply(*result);
              } else {
                completer.ReplyError(result.status());
              }
            }));
    return;
  }

  std::get<GpioClient>(impl_).buffer(arena)->ReleaseInterrupt(pin_).ThenExactlyOnce(
      fit::inline_callback<
          void(fdf::WireUnownedResult<fuchsia_hardware_gpioimpl::GpioImpl::ReleaseInterrupt>&),
          sizeof(ReleaseInterruptCompleter::Async)>(
          [completer = completer.ToAsync()](auto& result) mutable {
            if (!result.ok()) {
              completer.ReplyError(result.status());
            } else if (result->is_error()) {
              completer.ReplyError(result->error_value());
            } else {
              completer.ReplySuccess();
            }
          }));
}

void GpioDevice::SetAltFunction(SetAltFunctionRequestView request,
                                SetAltFunctionCompleter::Sync& completer) {
  fdf::Arena arena('GPIO');

  if (std::holds_alternative<PinClient>(impl_)) {
    auto config = fuchsia_hardware_pin::wire::Configuration::Builder(arena)
                      .function(request->function)
                      .Build();
    std::get<PinClient>(impl_)
        .buffer(arena)
        ->Configure(pin_, config)
        .ThenExactlyOnce(
            fit::inline_callback<
                void(fdf::WireUnownedResult<fuchsia_hardware_pinimpl::PinImpl::Configure>&),
                sizeof(SetAltFunctionCompleter::Async)>(
                [completer = completer.ToAsync()](auto& result) mutable {
                  if (!result.ok()) {
                    completer.ReplyError(result.status());
                  } else if (result->is_error()) {
                    completer.ReplyError(result->error_value());
                  } else {
                    completer.ReplySuccess();
                  }
                }));
    return;
  }

  std::get<GpioClient>(impl_)
      .buffer(arena)
      ->SetAltFunction(pin_, request->function)
      .ThenExactlyOnce(
          fit::inline_callback<
              void(fdf::WireUnownedResult<fuchsia_hardware_gpioimpl::GpioImpl::SetAltFunction>&),
              sizeof(SetAltFunctionCompleter::Async)>(
              [completer = completer.ToAsync()](auto& result) mutable {
                if (!result.ok()) {
                  completer.ReplyError(result.status());
                } else if (result->is_error()) {
                  completer.ReplyError(result->error_value());
                } else {
                  completer.ReplySuccess();
                }
              }));
}

void GpioDevice::handle_unknown_method(
    fidl::UnknownMethodMetadata<fuchsia_hardware_gpio::Gpio> metadata,
    fidl::UnknownMethodCompleter::Sync& completer) {
  FDF_LOG(ERROR, "Unknown Gpio method ordinal 0x%016lx", metadata.method_ordinal);
}

zx::result<> GpioDevice::AddServices(const std::shared_ptr<fdf::Namespace>& incoming,
                                     const std::shared_ptr<fdf::OutgoingDirectory>& outgoing,
                                     const std::optional<std::string>& node_name) {
  zx::result<> compat_result = compat_server_.Initialize(incoming, outgoing, node_name, pin_name());
  if (compat_result.is_error()) {
    FDF_LOG(ERROR, "Failed to initialize compat server: %s", compat_result.status_string());
    return compat_result.take_error();
  }

  fuchsia_hardware_gpio::Service::InstanceHandler handler({
      .device =
          [&](fidl::ServerEnd<fuchsia_hardware_gpio::Gpio> server) {
            async::PostTask(fidl_dispatcher_, [this, server = std::move(server)]() mutable {
              bindings_.AddBinding(fidl_dispatcher_, std::move(server), this,
                                   fidl::kIgnoreBindingClosure);
            });
          },
  });
  zx::result<> service_result =
      outgoing->AddService<fuchsia_hardware_gpio::Service>(std::move(handler), pin_name());
  if (service_result.is_error()) {
    FDF_LOG(ERROR, "Failed to add service to the outgoing directory");
    return service_result.take_error();
  }

  return zx::ok();
}

zx::result<> GpioDevice::AddDevice(fidl::UnownedClientEnd<fuchsia_driver_framework::Node> root_node,
                                   fdf::Logger& logger) {
  std::vector<fuchsia_driver_framework::Offer> offers = compat_server_.CreateOffers2();
  offers.push_back(fdf::MakeOffer2<fuchsia_hardware_gpio::Service>(pin_name()));

  std::vector<fuchsia_driver_framework::NodeProperty> props{
      fdf::MakeProperty(bind_fuchsia::GPIO_PIN, pin_),
      fdf::MakeProperty(bind_fuchsia::GPIO_CONTROLLER, controller_id_),
  };

  zx::result connector = devfs_connector_.Bind(fidl_dispatcher_);

  fuchsia_driver_framework::DevfsAddArgs devfs{{
      .connector = *std::move(connector),
      .class_name = "gpio",
      .connector_supports = fuchsia_device_fs::ConnectionType::kDevice,
  }};

  zx::result<fidl::ClientEnd<fuchsia_driver_framework::NodeController>> result =
      fdf::AddChild(root_node, logger, pin_name(), devfs, props, offers);
  if (result.is_error()) {
    FDF_LOG(ERROR, "AddChild failed for pin %u", pin_);
    return result.take_error();
  }

  controller_ = *std::move(result);
  return zx::ok();
}

void GpioDevice::DevfsConnect(fidl::ServerEnd<fuchsia_hardware_gpio::Gpio> server) {
  bindings_.AddBinding(fidl_dispatcher_, std::move(server), this, fidl::kIgnoreBindingClosure);
}

void GpioRootDevice::Start(fdf::StartCompleter completer) {
  uint32_t controller_id = 0;

  fidl::Arena arena;

  zx::result decoded = compat::GetMetadata<fuchsia_hardware_pinimpl::wire::ControllerMetadata>(
      incoming(), arena, DEVICE_METADATA_GPIO_CONTROLLER);
  if (decoded.is_error()) {
    if (decoded.status_value() == ZX_ERR_NOT_FOUND) {
      FDF_LOG(INFO, "No gpio controller metadata provided. Assuming controller id = 0.");
    } else {
      FDF_LOG(ERROR, "Failed to decode metadata: %s", decoded.status_string());
      return completer(decoded.take_error());
    }
  } else {
    controller_id = decoded->id;
  }

  zx::result scheduler_role = compat::GetMetadata<fuchsia_scheduler::RoleName>(
      incoming(), DEVICE_METADATA_SCHEDULER_ROLE_NAME);
  if (scheduler_role.is_ok()) {
    zx::result result = fdf::SynchronizedDispatcher::Create(
        {}, "GPIO", [](fdf_dispatcher_t*) {}, scheduler_role->role());
    if (result.is_error()) {
      FDF_LOG(ERROR, "Failed to create SynchronizedDispatcher: %s", result.status_string());
      return completer(result.take_error());
    }

    // If scheduler role metadata was provided, create a new dispatcher using the role, and use
    // that dispatcher instead of the default dispatcher passed to this method.
    fidl_dispatcher_.emplace(*std::move(result));

    FDF_LOG(DEBUG, "Using dispatcher with role \"%s\"", scheduler_role->role().c_str());
  }

  zx::result gpio_fidl_client = incoming()->Connect<fuchsia_hardware_gpioimpl::Service::Device>();
  if (gpio_fidl_client.is_ok()) {
    // Make a synchronous call on the gpioimpl client to verify that we are talking to a gpioimpl
    // driver. If not, fall back to pinimpl instead. gpioimpl is checked because it has a method
    // with no side effects (GetControllerId).
    fdf::Arena gpioimpl_arena('GPIO');
    auto result = fdf::WireCall(*gpio_fidl_client).buffer(gpioimpl_arena)->GetControllerId();
    if (result.ok()) {
      gpio_.Bind(
          *std::move(gpio_fidl_client), fidl_dispatcher()->get(),
          fidl::ObserveTeardown(fit::bind_member<&GpioRootDevice::ClientTeardownHandler>(this)));

      init_device_ =
          GpioInitDevice::Create(incoming(), node().borrow(), logger(), controller_id, gpio_);
    }
  }

  if (!gpio_.is_valid()) {
    zx::result pinimpl_fidl_client =
        incoming()->Connect<fuchsia_hardware_pinimpl::Service::Device>();
    if (pinimpl_fidl_client.is_error()) {
      FDF_LOG(ERROR, "Failed to get pinimpl protocol");
      return completer(pinimpl_fidl_client.take_error());
    }

    pinimpl_.Bind(
        *std::move(pinimpl_fidl_client), fidl_dispatcher()->get(),
        fidl::ObserveTeardown(fit::bind_member<&GpioRootDevice::ClientTeardownHandler>(this)));

    // Process init metadata while we are still the exclusive owner of the GPIO client.
    init_device_ =
        GpioInitDevice::Create(incoming(), node().borrow(), logger(), controller_id, pinimpl_);
  }

  if (zx::result<fdf::OwnedChildNode> node = AddOwnedChild("gpio"); node.is_error()) {
    FDF_LOG(ERROR, "Failed to add GPIO root node: %s", node.status_string());
    return completer(node.take_error());
  } else {
    node_ = *std::move(node);
  }

  zx::result pins = compat::GetMetadataArray<gpio_pin_t>(incoming(), DEVICE_METADATA_GPIO_PINS);
  if (pins.is_error()) {
    if (pins.status_value() == ZX_ERR_NOT_FOUND) {
      FDF_LOG(INFO, "No pins metadata provided");
    } else {
      FDF_LOG(ERROR, "Failed to get metadata array: %s", pins.status_string());
      return completer(pins.take_error());
    }
    completer(zx::ok());
  } else {
    // Make sure that the list of GPIO pins has no duplicates.
    auto gpio_cmp_lt = [](gpio_pin_t& lhs, gpio_pin_t& rhs) { return lhs.pin < rhs.pin; };
    auto gpio_cmp_eq = [](gpio_pin_t& lhs, gpio_pin_t& rhs) { return lhs.pin == rhs.pin; };
    std::sort(pins.value().begin(), pins.value().end(), gpio_cmp_lt);
    auto result = std::adjacent_find(pins.value().begin(), pins.value().end(), gpio_cmp_eq);
    if (result != pins.value().end()) {
      FDF_LOG(ERROR, "gpio pin '%d' was published more than once", result->pin);
      return completer(zx::error(ZX_ERR_INVALID_ARGS));
    }

    async::PostTask(fidl_dispatcher()->async_dispatcher(),
                    [=, pins = *std::move(pins), completer = std::move(completer)]() mutable {
                      CreatePinDevices(controller_id, pins, std::move(completer));
                    });
  }
}

void GpioRootDevice::PrepareStop(fdf::PrepareStopCompleter completer) {
  ZX_DEBUG_ASSERT(!stop_completer_);
  stop_completer_.emplace(std::move(completer));
  if (gpio_.is_valid()) {
    gpio_.AsyncTeardown();
  }
  if (pinimpl_.is_valid()) {
    pinimpl_.AsyncTeardown();
  }
}

void GpioRootDevice::CreatePinDevices(const uint32_t controller_id,
                                      const std::vector<gpio_pin_t>& pins,
                                      fdf::StartCompleter completer) {
  for (const auto& pin : pins) {
    fbl::AllocChecker ac;

    if (pinimpl_.is_valid()) {
      children_.emplace_back(new (&ac)
                                 GpioDevice(pinimpl_.Clone(), pin.pin, controller_id, pin.name));
    } else {
      children_.emplace_back(new (&ac) GpioDevice(gpio_.Clone(), pin.pin, controller_id, pin.name));
    }

    if (!ac.check()) {
      return completer(zx::error(ZX_ERR_NO_MEMORY));
    }
  }

  async::PostTask(dispatcher(), [=, completer = std::move(completer)]() mutable {
    ServePinDevices(std::move(completer));
  });
}

void GpioRootDevice::ServePinDevices(fdf::StartCompleter completer) {
  for (std::unique_ptr<GpioDevice>& child : children_) {
    zx::result<> result = child->AddServices(incoming(), outgoing(), node_name());
    if (result.is_error()) {
      return completer(result);
    }
  }

  async::PostTask(
      fidl_dispatcher()->async_dispatcher(),
      [=, completer = std::move(completer)]() mutable { AddPinDevices(std::move(completer)); });
}

void GpioRootDevice::AddPinDevices(fdf::StartCompleter completer) {
  for (std::unique_ptr<GpioDevice>& child : children_) {
    if (zx::result<> result = child->AddDevice(node_.node_.borrow(), logger()); result.is_error()) {
      return completer(result);
    }
  }

  completer(zx::ok());
}

void GpioRootDevice::ClientTeardownHandler() {
  async::PostTask(dispatcher(), [this]() {
    if (stop_completer_) {
      (*stop_completer_)(zx::ok());
    }
  });
}

template <typename T>
std::unique_ptr<GpioInitDevice> GpioInitDevice::Create(
    const std::shared_ptr<fdf::Namespace>& incoming,
    fidl::UnownedClientEnd<fuchsia_driver_framework::Node> node, fdf::Logger& logger,
    uint32_t controller_id, fdf::WireSharedClient<T>& impl) {
  // Don't add the init device if anything goes wrong here, as the hardware may be in a state that
  // child devices don't expect.
  fdf::Arena arena('GPIO');
  zx::result decoded = compat::GetMetadata<fuchsia_hardware_pinimpl::wire::Metadata>(
      incoming, arena, DEVICE_METADATA_GPIO_INIT);
  if (!decoded.is_ok()) {
    if (decoded.status_value() == ZX_ERR_NOT_FOUND) {
      FDF_LOG(INFO, "No init metadata provided");
    } else {
      FDF_LOG(ERROR, "Failed to decode metadata: %s", decoded.status_string());
    }
    return {};
  }
  if (!(*decoded)->has_init_steps()) {
    FDF_LOG(INFO, "No init metadata provided");
    return {};
  }

  std::unique_ptr device = std::make_unique<GpioInitDevice>();
  if (device->ConfigureGpios((*decoded)->init_steps(), impl) != ZX_OK) {
    // Return without adding the init device if some GPIOs could not be configured. This will
    // prevent all drivers that depend on the initial state from binding, which should make it more
    // obvious that something has gone wrong.
    return {};
  }

  std::vector<fuchsia_driver_framework::NodeProperty> props{
      fdf::MakeProperty(bind_fuchsia::INIT_STEP, bind_fuchsia_gpio::BIND_INIT_STEP_GPIO),
      fdf::MakeProperty(bind_fuchsia::GPIO_CONTROLLER, controller_id),
  };

  zx::result<fidl::ClientEnd<fuchsia_driver_framework::NodeController>> result =
      fdf::AddChild(node, logger, "gpio-init", props, {});
  if (result.is_error()) {
    FDF_LOG(ERROR, "Failed to add gpio-init node: %s", result.status_string());
    return {};
  }

  device->controller_.Bind(*std::move(result));
  return device;
}

zx_status_t GpioInitDevice::ConfigureGpios(
    const fidl::VectorView<fuchsia_hardware_pinimpl::wire::InitStep>& init_steps,
    fdf::WireSharedClient<fuchsia_hardware_gpioimpl::GpioImpl>& gpio) {
  // Stop processing the list if any call returns an error so that GPIOs are not accidentally put
  // into an unexpected state.
  for (const auto& step : init_steps) {
    fdf::Arena arena('GPIO');

    if (step.is_delay()) {
      zx::nanosleep(zx::deadline_after(zx::duration(step.delay())));
      continue;
    }
    if (!step.is_call()) {
      FDF_LOG(ERROR, "Invalid GPIO init metadata");
      return ZX_ERR_INVALID_ARGS;
    }

    if (step.call().call.is_pin_config()) {
      const auto& config = step.call().call.pin_config();

      if (config.has_pull()) {
        auto result =
            gpio.sync().buffer(arena)->ConfigIn(step.call().pin, PullToGpioFlags(config.pull()));
        if (!result.ok()) {
          FDF_LOG(ERROR, "Call to ConfigIn failed: %s", result.status_string());
          return result.status();
        }
        if (result->is_error()) {
          FDF_LOG(ERROR, "ConfigIn(%u) failed for %u: %s", static_cast<uint32_t>(config.pull()),
                  step.call().pin, zx_status_get_string(result->error_value()));
          return result->error_value();
        }
      }

      if (config.has_function()) {
        auto result = gpio.sync().buffer(arena)->SetAltFunction(step.call().pin, config.function());
        if (!result.ok()) {
          FDF_LOG(ERROR, "Call to SetAltFunction failed: %s", result.status_string());
          return result.status();
        }
        if (result->is_error()) {
          FDF_LOG(ERROR, "SetAltFunction(%lu) failed for %u: %s", config.function(),
                  step.call().pin, zx_status_get_string(result->error_value()));
          return result->error_value();
        }
      }

      if (config.has_drive_strength_ua()) {
        auto result = gpio.sync().buffer(arena)->SetDriveStrength(step.call().pin,
                                                                  config.drive_strength_ua());
        if (!result.ok()) {
          FDF_LOG(ERROR, "Call to SetDriveStrength failed: %s", result.status_string());
          return result.status();
        }
        if (result->is_error()) {
          FDF_LOG(ERROR, "SetDriveStrength(%lu) failed for %u: %s", config.drive_strength_ua(),
                  step.call().pin, zx_status_get_string(result->error_value()));
          return result->error_value();
        }
        if (result->value()->actual_ds_ua != config.drive_strength_ua()) {
          FDF_LOG(WARNING, "Actual drive strength (%lu) doesn't match expected (%lu) for %u",
                  result->value()->actual_ds_ua, config.drive_strength_ua(), step.call().pin);
          return ZX_ERR_BAD_STATE;
        }
      }
    } else if (step.call().call.is_buffer_mode()) {
      // TODO(42082459): Support disabling output.
      ZX_DEBUG_ASSERT(step.call().call.buffer_mode() != fuchsia_hardware_gpio::BufferMode::kInput);

      const uint8_t value =
          step.call().call.buffer_mode() == fuchsia_hardware_gpio::BufferMode::kOutputHigh ? 1 : 0;
      auto result = gpio.sync().buffer(arena)->ConfigOut(step.call().pin, value);
      if (!result.ok()) {
        FDF_LOG(ERROR, "Call to ConfigOut failed: %s", result.status_string());
        return result.status();
      }
      if (result->is_error()) {
        FDF_LOG(ERROR, "ConfigOut(%u) failed for %u: %s", value, step.call().pin,
                zx_status_get_string(result->error_value()));
        return result->error_value();
      }
    }
  }

  return ZX_OK;
}

zx_status_t GpioInitDevice::ConfigureGpios(
    const fidl::VectorView<fuchsia_hardware_pinimpl::wire::InitStep>& init_steps,
    fdf::WireSharedClient<fuchsia_hardware_pinimpl::PinImpl>& pinimpl) {
  // Stop processing the list if any call returns an error so that GPIOs are not accidentally put
  // into an unexpected state.
  for (const auto& step : init_steps) {
    fdf::Arena arena('GPIO');

    if (step.is_delay()) {
      zx::nanosleep(zx::deadline_after(zx::duration(step.delay())));
      continue;
    }
    if (!step.is_call()) {
      FDF_LOG(ERROR, "Invalid GPIO init metadata");
      return ZX_ERR_INVALID_ARGS;
    }

    const uint32_t pin = step.call().pin;
    if (step.call().call.is_pin_config()) {
      const auto& config = step.call().call.pin_config();
      auto result = pinimpl.sync().buffer(arena)->Configure(pin, config);
      if (!result.ok()) {
        FDF_LOG(ERROR, "Call to Configure failed: %s", result.status_string());
        return result.status();
      }
      if (result->is_error()) {
        FDF_LOG(ERROR, "Configure failed for %u: %s", pin,
                zx_status_get_string(result->error_value()));
        return result->error_value();
      }

      if (config.has_drive_strength_ua()) {
        if (!result->value()->new_config.has_drive_strength_ua()) {
          FDF_LOG(WARNING, "Drive strength not returned for %u", pin);
          return ZX_ERR_BAD_STATE;
        }
        if (result->value()->new_config.drive_strength_ua() != config.drive_strength_ua()) {
          FDF_LOG(WARNING, "Actual drive strength (%lu) doesn't match expected (%lu) for %u",
                  result->value()->new_config.drive_strength_ua(), config.drive_strength_ua(), pin);
          return ZX_ERR_BAD_STATE;
        }
      }
    } else if (step.call().call.is_buffer_mode()) {
      auto result =
          pinimpl.sync().buffer(arena)->SetBufferMode(pin, step.call().call.buffer_mode());
      if (!result.ok()) {
        FDF_LOG(ERROR, "Call to SetBufferMode failed: %s", result.status_string());
        return result.status();
      }
      if (result->is_error()) {
        FDF_LOG(ERROR, "SetBufferMode failed for %u: %s", pin,
                zx_status_get_string(result->error_value()));
        return result->error_value();
      }
    }
  }

  return ZX_OK;
}

}  // namespace gpio

FUCHSIA_DRIVER_EXPORT(gpio::GpioRootDevice);
