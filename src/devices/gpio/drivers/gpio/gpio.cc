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

void GpioDevice::GetPin(GetPinCompleter::Sync& completer) { completer.ReplySuccess(pin_); }

void GpioDevice::GetName(GetNameCompleter::Sync& completer) {
  completer.ReplySuccess(fidl::StringView::FromExternal(name_));
}

void GpioDevice::ConfigIn(ConfigInRequestView request, ConfigInCompleter::Sync& completer) {
  fdf::Arena arena('GPIO');
  gpio_.buffer(arena)
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
  gpio_.buffer(arena)
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
  gpio_.buffer(arena)->Read(pin_).ThenExactlyOnce(
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
  gpio_.buffer(arena)
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
  gpio_.buffer(arena)
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
  gpio_.buffer(arena)->GetDriveStrength(pin_).ThenExactlyOnce(
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
  gpio_.buffer(arena)
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

void GpioDevice::ReleaseInterrupt(ReleaseInterruptCompleter::Sync& completer) {
  fdf::Arena arena('GPIO');
  gpio_.buffer(arena)->ReleaseInterrupt(pin_).ThenExactlyOnce(
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
  gpio_.buffer(arena)
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

void GpioDevice::SetPolarity(SetPolarityRequestView request,
                             SetPolarityCompleter::Sync& completer) {
  fdf::Arena arena('GPIO');
  gpio_.buffer(arena)
      ->SetPolarity(pin_, request->polarity)
      .ThenExactlyOnce(
          fit::inline_callback<
              void(fdf::WireUnownedResult<fuchsia_hardware_gpioimpl::GpioImpl::SetPolarity>&),
              sizeof(SetPolarityCompleter::Async)>(
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

  zx::result decoded = compat::GetMetadata<fuchsia_hardware_gpioimpl::wire::ControllerMetadata>(
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

  {
    zx::result gpio_fidl_client = incoming()->Connect<fuchsia_hardware_gpioimpl::Service::Device>();
    if (gpio_fidl_client.is_error()) {
      FDF_LOG(ERROR, "Failed to get gpioimpl protocol");
      return completer(gpio_fidl_client.take_error());
    }

    gpio_.Bind(
        *std::move(gpio_fidl_client), fidl_dispatcher()->get(),
        fidl::ObserveTeardown(fit::bind_member<&GpioRootDevice::ClientTeardownHandler>(this)));

    // Process init metadata while we are still the exclusive owner of the GPIO client.
    init_device_ =
        GpioInitDevice::Create(incoming(), node().borrow(), logger(), controller_id, gpio_);
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
  gpio_.AsyncTeardown();
}

void GpioRootDevice::CreatePinDevices(const uint32_t controller_id,
                                      const std::vector<gpio_pin_t>& pins,
                                      fdf::StartCompleter completer) {
  for (const auto& pin : pins) {
    fbl::AllocChecker ac;
    children_.emplace_back(new (&ac) GpioDevice(gpio_.Clone(), pin.pin, controller_id, pin.name));
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

std::unique_ptr<GpioInitDevice> GpioInitDevice::Create(
    const std::shared_ptr<fdf::Namespace>& incoming,
    fidl::UnownedClientEnd<fuchsia_driver_framework::Node> node, fdf::Logger& logger,
    uint32_t controller_id, fdf::WireSharedClient<fuchsia_hardware_gpioimpl::GpioImpl>& gpio) {
  // Don't add the init device if anything goes wrong here, as the hardware may be in a state that
  // child devices don't expect.
  fdf::Arena arena('GPIO');
  zx::result decoded = compat::GetMetadata<fuchsia_hardware_gpioimpl::wire::InitMetadata>(
      incoming, arena, DEVICE_METADATA_GPIO_INIT);
  if (!decoded.is_ok()) {
    if (decoded.status_value() == ZX_ERR_NOT_FOUND) {
      FDF_LOG(INFO, "No init metadata provided");
    } else {
      FDF_LOG(ERROR, "Failed to decode metadata: %s", decoded.status_string());
    }
    return {};
  }

  std::unique_ptr device = std::make_unique<GpioInitDevice>();
  if (device->ConfigureGpios(**decoded, gpio) != ZX_OK) {
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
    const fuchsia_hardware_gpioimpl::wire::InitMetadata& metadata,
    fdf::WireSharedClient<fuchsia_hardware_gpioimpl::GpioImpl>& gpio) {
  // Stop processing the list if any call returns an error so that GPIOs are not accidentally put
  // into an unexpected state.
  for (const auto& step : metadata.steps) {
    fdf::Arena arena('GPIO');

    if (!step.has_call()) {
      FDF_LOG(ERROR, "GPIO Init Metadata step is missing a call field");
      return ZX_ERR_INVALID_ARGS;
    }

    // Delay doesn't apply to any particular gpio ID so we enforce that the ID field is
    // unset. Every other type of init call requires an ID so we enforce that ID is set.
    if (step.call().is_delay() && step.has_index()) {
      FDF_LOG(ERROR, "GPIO Init Delay calls must not have an ID, id = %u", step.index());
      return ZX_ERR_INVALID_ARGS;
    }
    if (!step.call().is_delay() && !step.has_index()) {
      FDF_LOG(ERROR, "GPIO init calls must have an ID");
      return ZX_ERR_INVALID_ARGS;
    }

    if (step.call().is_input_flags()) {
      auto result = gpio.sync().buffer(arena)->ConfigIn(step.index(), step.call().input_flags());
      if (!result.ok()) {
        FDF_LOG(ERROR, "Call to ConfigIn failed: %s", result.status_string());
        return result.status();
      }
      if (result->is_error()) {
        FDF_LOG(ERROR, "ConfigIn(%u) failed for %u: %s",
                static_cast<uint32_t>(step.call().input_flags()), step.index(),
                zx_status_get_string(result->error_value()));
        return result->error_value();
      }
    } else if (step.call().is_output_value()) {
      auto result = gpio.sync().buffer(arena)->ConfigOut(step.index(), step.call().output_value());
      if (!result.ok()) {
        FDF_LOG(ERROR, "Call to ConfigOut failed: %s", result.status_string());
        return result.status();
      }
      if (result->is_error()) {
        FDF_LOG(ERROR, "ConfigOut(%u) failed for %u: %s", step.call().output_value(), step.index(),
                zx_status_get_string(result->error_value()));
        return result->error_value();
      }
    } else if (step.call().is_alt_function()) {
      auto result =
          gpio.sync().buffer(arena)->SetAltFunction(step.index(), step.call().alt_function());
      if (!result.ok()) {
        FDF_LOG(ERROR, "Call to SetAltFunction failed: %s", result.status_string());
        return result.status();
      }
      if (result->is_error()) {
        FDF_LOG(ERROR, "SetAltFunction(%lu) failed for %u: %s", step.call().alt_function(),
                step.index(), zx_status_get_string(result->error_value()));
        return result->error_value();
      }
    } else if (step.call().is_drive_strength_ua()) {
      auto result = gpio.sync().buffer(arena)->SetDriveStrength(step.index(),
                                                                step.call().drive_strength_ua());
      if (!result.ok()) {
        FDF_LOG(ERROR, "Call to SetDriveStrength failed: %s", result.status_string());
        return result.status();
      }
      if (result->is_error()) {
        FDF_LOG(ERROR, "SetDriveStrength(%lu) failed for %u: %s", step.call().drive_strength_ua(),
                step.index(), zx_status_get_string(result->error_value()));
        return result->error_value();
      }
      if (result->value()->actual_ds_ua != step.call().drive_strength_ua()) {
        FDF_LOG(WARNING, "Actual drive strength (%lu) doesn't match expected (%lu) for %u",
                result->value()->actual_ds_ua, step.call().drive_strength_ua(), step.index());
        return ZX_ERR_BAD_STATE;
      }
    } else if (step.call().is_delay()) {
      zx::nanosleep(zx::deadline_after(zx::duration(step.call().delay())));
    }
  }

  return ZX_OK;
}

}  // namespace gpio

FUCHSIA_DRIVER_EXPORT(gpio::GpioRootDevice);
