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

#include <algorithm>
#include <memory>

#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/gpio/cpp/bind.h>
#include <fbl/alloc_checker.h>

namespace gpio {

void GpioDevice::GpioInstance::Read(ReadCompleter::Sync& completer) {
  fdf::Arena arena('GPIO');
  pinimpl_.buffer(arena)->Read(pin_).ThenExactlyOnce(
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
}

void GpioDevice::GpioInstance::SetBufferMode(SetBufferModeRequestView request,
                                             SetBufferModeCompleter::Sync& completer) {
  fdf::Arena arena('GPIO');
  pinimpl_.buffer(arena)
      ->SetBufferMode(pin_, request->mode)
      .ThenExactlyOnce(
          fit::inline_callback<
              void(fdf::WireUnownedResult<fuchsia_hardware_pinimpl::PinImpl::SetBufferMode>&),
              sizeof(SetBufferModeCompleter::Async)>(
              [completer = completer.ToAsync()](auto& result) mutable {
                if (result.ok()) {
                  completer.Reply(*result);
                } else {
                  completer.ReplyError(result.status());
                }
              }));
}

void GpioDevice::GpioInstance::GetInterrupt(GetInterruptRequestView request,
                                            GetInterruptCompleter::Sync& completer) {
  if (has_interrupt()) {
    completer.ReplyError(ZX_ERR_ALREADY_EXISTS);
    return;
  }
  if (parent_->gpio_instance_has_interrupt()) {
    completer.ReplyError(ZX_ERR_ACCESS_DENIED);
    return;
  }

  interrupt_state_ = InterruptState::kGettingInterrupt;

  fdf::Arena arena('GPIO');
  pinimpl_.buffer(arena)
      ->GetInterrupt(pin_, request->options)
      .ThenExactlyOnce(
          [instance = fbl::RefPtr(this), completer = completer.ToAsync()](auto& result) mutable {
            // Clear ownership of the interrupt if the call failed.
            if (!result.ok()) {
              instance->interrupt_state_ = InterruptState::kNoInterrupt;
              completer.ReplyError(result.status());
            } else if (result->is_error()) {
              instance->interrupt_state_ = InterruptState::kNoInterrupt;
              completer.ReplyError(result->error_value());
            } else {
              instance->interrupt_state_ = InterruptState::kHasInterrupt;
              completer.ReplySuccess(std::move(result->value()->interrupt));
            }

            if (instance->release_instance_after_call_completes_) {
              instance->ReleaseInstance();
            }
          });
}

void GpioDevice::GpioInstance::ConfigureInterrupt(
    fuchsia_hardware_gpio::wire::GpioConfigureInterruptRequest* request,
    ConfigureInterruptCompleter::Sync& completer) {
  if (parent_->gpio_instance_has_interrupt() && !has_interrupt()) {
    // Allow the interrupt to be configured if we own it, or if no instance has one.
    completer.ReplyError(ZX_ERR_ACCESS_DENIED);
    return;
  }

  fdf::Arena arena('GPIO');
  pinimpl_.buffer(arena)
      ->ConfigureInterrupt(pin_, request->config)
      .ThenExactlyOnce(
          fit::inline_callback<
              void(fdf::WireUnownedResult<fuchsia_hardware_pinimpl::PinImpl::ConfigureInterrupt>&),
              sizeof(ConfigureInterruptCompleter::Async)>(
              [completer = completer.ToAsync()](auto& result) mutable {
                if (result.ok()) {
                  completer.Reply(*result);
                } else {
                  completer.ReplyError(result.status());
                }
              }));
}

void GpioDevice::GpioInstance::ReleaseInterrupt(ReleaseInterruptCompleter::Sync& completer) {
  if (parent_->gpio_instance_has_interrupt() && !has_interrupt()) {
    completer.ReplyError(ZX_ERR_ACCESS_DENIED);
    return;
  }
  if (interrupt_state_ != InterruptState::kHasInterrupt) {
    // We might be in the process of getting the interrupt now, but we haven't returned it to the
    // client yet.
    completer.ReplyError(ZX_ERR_NOT_FOUND);
    return;
  }

  interrupt_state_ = InterruptState::kReleasingInterrupt;

  fdf::Arena arena('GPIO');
  pinimpl_.buffer(arena)->ReleaseInterrupt(pin_).ThenExactlyOnce(
      [instance = fbl::RefPtr(this), completer = completer.ToAsync()](auto& result) mutable {
        if (result.ok()) {
          completer.Reply(*result);
        } else {
          completer.ReplyError(result.status());
        }

        instance->interrupt_state_ = InterruptState::kNoInterrupt;
        if (instance->release_instance_after_call_completes_) {
          instance->ReleaseInstance();
        }
      });
}

void GpioDevice::GpioInstance::handle_unknown_method(
    fidl::UnknownMethodMetadata<fuchsia_hardware_gpio::Gpio> metadata,
    fidl::UnknownMethodCompleter::Sync& completer) {
  FDF_LOG(ERROR, "Unknown Gpio method ordinal 0x%016lx", metadata.method_ordinal);
}

void GpioDevice::GpioInstance::OnUnbound(fidl::UnbindInfo info) {
  if (interrupt_state_ == InterruptState::kHasInterrupt ||
      interrupt_state_ == InterruptState::kNoInterrupt) {
    // There are no calls pending, so release the interrupt if there is one, then tell the parent to
    // release us.
    ReleaseInstance();
  } else {
    // A call is pending -- set release_instance_after_call_completes_ and wait for the call to
    // complete.
    release_instance_after_call_completes_ = true;
  }
}

void GpioDevice::GpioInstance::ReleaseInstance() {
  release_instance_after_call_completes_ = false;
  if (interrupt_state_ == InterruptState::kHasInterrupt) {
    interrupt_state_ = InterruptState::kReleasingInterrupt;

    fdf::Arena arena('GPIO');
    pinimpl_.buffer(arena)->ReleaseInterrupt(pin_).Then(
        [instance = fbl::RefPtr(this)](auto& result) {
          instance->interrupt_state_ = InterruptState::kNoInterrupt;
          instance->RemoveFromContainer();
        });
  } else if (interrupt_state_ == InterruptState::kNoInterrupt) {
    RemoveFromContainer();
  } else {
    ZX_DEBUG_ASSERT_MSG(false, "ReleaseInstance called in an invalid state");
  }
}

bool GpioDevice::gpio_instance_has_interrupt() const {
  return std::any_of(gpio_instances_.cbegin(), gpio_instances_.cend(),
                     [](const GpioInstance& instance) { return instance.has_interrupt(); });
}

void GpioDevice::Configure(fuchsia_hardware_pin::wire::PinConfigureRequest* request,
                           ConfigureCompleter::Sync& completer) {
  fdf::Arena arena('GPIO');
  pinimpl_.buffer(arena)
      ->Configure(pin_, request->config)
      .ThenExactlyOnce(fit::inline_callback<
                       void(fdf::WireUnownedResult<fuchsia_hardware_pinimpl::PinImpl::Configure>&),
                       sizeof(ConfigureCompleter::Async)>(
          [completer = completer.ToAsync()](auto& result) mutable {
            if (!result.ok()) {
              completer.ReplyError(result.status());
            } else if (result->is_error()) {
              completer.ReplyError(result->error_value());
            } else {
              completer.ReplySuccess(result->value()->new_config);
            }
          }));
}

void GpioDevice::handle_unknown_method(
    fidl::UnknownMethodMetadata<fuchsia_hardware_pin::Pin> metadata,
    fidl::UnknownMethodCompleter::Sync& completer) {
  FDF_LOG(ERROR, "Unknown Pin method ordinal: 0x%016lx", metadata.method_ordinal);
}

void GpioDevice::GetProperties(GetPropertiesCompleter::Sync& completer) {
  fdf::Arena arena('GPIO');
  auto properties = fuchsia_hardware_pin::wire::DebugGetPropertiesResponse::Builder(arena)
                        .name(fidl::StringView::FromExternal(name_))
                        .pin(pin_)
                        .Build();
  completer.Reply(properties);
}

void GpioDevice::ConnectPin(fuchsia_hardware_pin::wire::DebugConnectPinRequest* request,
                            ConnectPinCompleter::Sync& completer) {
  pin_bindings_.AddBinding(fidl_dispatcher_, std::move(request->server), this,
                           fidl::kIgnoreBindingClosure);
  completer.ReplySuccess();
}

void GpioDevice::ConnectGpio(fuchsia_hardware_pin::wire::DebugConnectGpioRequest* request,
                             ConnectGpioCompleter::Sync& completer) {
  ConnectGpio(std::move(request->server));
  completer.ReplySuccess();
}

void GpioDevice::handle_unknown_method(
    fidl::UnknownMethodMetadata<fuchsia_hardware_pin::Debug> metadata,
    fidl::UnknownMethodCompleter::Sync& completer) {
  FDF_LOG(ERROR, "Unknown Debug method ordinal: 0x%016lx", metadata.method_ordinal);
}

void GpioDevice::ConnectGpio(fidl::ServerEnd<fuchsia_hardware_gpio::Gpio> server) {
  gpio_instances_.push_front(fbl::MakeRefCounted<GpioInstance>(fidl_dispatcher_, std::move(server),
                                                               pinimpl_.Clone(), pin_, this));
}

zx::result<> GpioDevice::AddServices(const std::shared_ptr<fdf::Namespace>& incoming,
                                     const std::shared_ptr<fdf::OutgoingDirectory>& outgoing,
                                     const std::optional<std::string>& node_name) {
  zx::result<> compat_result = compat_server_.Initialize(incoming, outgoing, node_name, pin_name());
  if (compat_result.is_error()) {
    FDF_LOG(ERROR, "Failed to initialize compat server: %s", compat_result.status_string());
    return compat_result.take_error();
  }

  fuchsia_hardware_gpio::Service::InstanceHandler gpio_handler({
      .device =
          [&](fidl::ServerEnd<fuchsia_hardware_gpio::Gpio> server) {
            async::PostTask(fidl_dispatcher_, [this, server = std::move(server)]() mutable {
              ConnectGpio(std::move(server));
            });
          },
  });
  zx::result<> service_result =
      outgoing->AddService<fuchsia_hardware_gpio::Service>(std::move(gpio_handler), pin_name());
  if (service_result.is_error()) {
    FDF_LOG(ERROR, "Failed to add Gpio service to the outgoing directory");
    return service_result.take_error();
  }

  fuchsia_hardware_pin::Service::InstanceHandler pin_handler({
      .device =
          [&](fidl::ServerEnd<fuchsia_hardware_pin::Pin> server) {
            async::PostTask(fidl_dispatcher_, [this, server = std::move(server)]() mutable {
              pin_bindings_.AddBinding(fidl_dispatcher_, std::move(server), this,
                                       fidl::kIgnoreBindingClosure);
            });
          },
  });
  service_result =
      outgoing->AddService<fuchsia_hardware_pin::Service>(std::move(pin_handler), pin_name());
  if (service_result.is_error()) {
    FDF_LOG(ERROR, "Failed to add Pin service to the outgoing directory");
    return service_result.take_error();
  }

  return zx::ok();
}

zx::result<> GpioDevice::AddDevice(fidl::UnownedClientEnd<fuchsia_driver_framework::Node> root_node,
                                   fdf::Logger& logger) {
  std::vector<fuchsia_driver_framework::Offer> offers = compat_server_.CreateOffers2();
  offers.push_back(fdf::MakeOffer2<fuchsia_hardware_gpio::Service>(pin_name()));
  offers.push_back(fdf::MakeOffer2<fuchsia_hardware_pin::Service>(pin_name()));

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

void GpioDevice::DevfsConnect(fidl::ServerEnd<fuchsia_hardware_pin::Debug> server) {
  debug_bindings_.AddBinding(fidl_dispatcher_, std::move(server), this,
                             fidl::kIgnoreBindingClosure);
}

void GpioRootDevice::Start(fdf::StartCompleter completer) {
  uint32_t controller_id = 0;

  fidl::Arena arena;

  zx::result decoded = compat::GetMetadata<fuchsia_hardware_pinimpl::Metadata>(
      incoming(), DEVICE_METADATA_GPIO_CONTROLLER);
  if (decoded.is_error()) {
    if (decoded.status_value() == ZX_ERR_NOT_FOUND) {
      FDF_LOG(INFO, "No gpio controller metadata provided. Assuming controller id = 0.");
    } else {
      FDF_LOG(ERROR, "Failed to decode metadata: %s", decoded.status_string());
      completer(decoded.take_error());
      return;
    }
  } else if (decoded->controller_id().has_value()) {
    controller_id = decoded->controller_id().value();
  }

  zx::result scheduler_role = compat::GetMetadata<fuchsia_scheduler::RoleName>(
      incoming(), DEVICE_METADATA_SCHEDULER_ROLE_NAME);
  if (scheduler_role.is_ok()) {
    zx::result result = fdf::SynchronizedDispatcher::Create(
        {}, "GPIO", [](fdf_dispatcher_t*) {}, scheduler_role->role());
    if (result.is_error()) {
      FDF_LOG(ERROR, "Failed to create SynchronizedDispatcher: %s", result.status_string());
      completer(result.take_error());
      return;
    }

    // If scheduler role metadata was provided, create a new dispatcher using the role, and use
    // that dispatcher instead of the default dispatcher passed to this method.
    fidl_dispatcher_.emplace(*std::move(result));

    FDF_LOG(DEBUG, "Using dispatcher with role \"%s\"", scheduler_role->role().c_str());
  }

  {
    zx::result pinimpl_fidl_client =
        incoming()->Connect<fuchsia_hardware_pinimpl::Service::Device>();
    if (pinimpl_fidl_client.is_error()) {
      FDF_LOG(ERROR, "Failed to get pinimpl protocol");
      completer(pinimpl_fidl_client.take_error());
      return;
    }

    pinimpl_.Bind(
        *std::move(pinimpl_fidl_client), fidl_dispatcher()->get(),
        fidl::ObserveTeardown(fit::bind_member<&GpioRootDevice::ClientTeardownHandler>(this)));

    if (decoded.is_ok() && decoded->init_steps().has_value() && !decoded->init_steps()->empty()) {
      // Process init metadata while we are still the exclusive owner of the GPIO client.
      init_device_ = GpioInitDevice::Create(decoded->init_steps().value(), node().borrow(),
                                            logger(), controller_id, pinimpl_);
    } else {
      FDF_LOG(INFO, "No init metadata provided");
    }
  }

  zx::result<fdf::OwnedChildNode> node = AddOwnedChild("gpio");
  if (node.is_error()) {
    FDF_LOG(ERROR, "Failed to add GPIO root node: %s", node.status_string());
    completer(node.take_error());
    return;
  }
  node_ = *std::move(node);

  zx::result pins = compat::GetMetadataArray<gpio_pin_t>(incoming(), DEVICE_METADATA_GPIO_PINS);
  if (pins.is_error()) {
    if (pins.status_value() == ZX_ERR_NOT_FOUND) {
      FDF_LOG(INFO, "No pins metadata provided");
    } else {
      FDF_LOG(ERROR, "Failed to get metadata array: %s", pins.status_string());
      completer(pins.take_error());
      return;
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
      completer(zx::error(ZX_ERR_INVALID_ARGS));
      return;
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
  pinimpl_.AsyncTeardown();
}

void GpioRootDevice::CreatePinDevices(const uint32_t controller_id,
                                      const std::vector<gpio_pin_t>& pins,
                                      fdf::StartCompleter completer) {
  for (const auto& pin : pins) {
    fbl::AllocChecker ac;
    children_.emplace_back(new (&ac)
                               GpioDevice(pinimpl_.Clone(), pin.pin, controller_id, pin.name));
    if (!ac.check()) {
      completer(zx::error(ZX_ERR_NO_MEMORY));
      return;
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
      completer(result);
      return;
    }
  }

  async::PostTask(
      fidl_dispatcher()->async_dispatcher(),
      [=, completer = std::move(completer)]() mutable { AddPinDevices(std::move(completer)); });
}

void GpioRootDevice::AddPinDevices(fdf::StartCompleter completer) {
  for (std::unique_ptr<GpioDevice>& child : children_) {
    if (zx::result<> result = child->AddDevice(node_.node_.borrow(), logger()); result.is_error()) {
      completer(result);
      return;
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
    std::span<fuchsia_hardware_pinimpl::InitStep> init_steps,
    fidl::UnownedClientEnd<fuchsia_driver_framework::Node> node, fdf::Logger& logger,
    uint32_t controller_id, fdf::WireSharedClient<fuchsia_hardware_pinimpl::PinImpl>& pinimpl) {
  std::unique_ptr device = std::make_unique<GpioInitDevice>();
  if (ConfigureGpios(init_steps, pinimpl) != ZX_OK) {
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
    std::span<fuchsia_hardware_pinimpl::InitStep> init_steps,
    fdf::WireSharedClient<fuchsia_hardware_pinimpl::PinImpl>& pinimpl) {
  // Stop processing the list if any call returns an error so that GPIOs are not accidentally put
  // into an unexpected state.
  for (const auto& step : init_steps) {
    fdf::Arena arena('GPIO');

    if (step.Which() == fuchsia_hardware_pinimpl::InitStep::Tag::kDelay) {
      zx::nanosleep(zx::deadline_after(zx::duration(step.delay().value())));
      continue;
    }
    if (step.Which() != fuchsia_hardware_pinimpl::InitStep::Tag::kCall) {
      FDF_LOG(ERROR, "Invalid GPIO init metadata");
      return ZX_ERR_INVALID_ARGS;
    }

    const auto& call = step.call()->call();
    const uint32_t pin = step.call()->pin();
    if (call.Which() == fuchsia_hardware_pinimpl::InitCall::Tag::kPinConfig) {
      const auto& config = call.pin_config().value();
      auto result = pinimpl.sync().buffer(arena)->Configure(pin, fidl::ToWire(arena, config));
      if (!result.ok()) {
        FDF_LOG(ERROR, "Call to Configure failed: %s", result.status_string());
        return result.status();
      }
      if (result->is_error()) {
        FDF_LOG(ERROR, "Configure failed for %u: %s", pin,
                zx_status_get_string(result->error_value()));
        return result->error_value();
      }

      const auto& driver_strength_ua = config.drive_strength_ua();
      if (driver_strength_ua.has_value()) {
        if (!result->value()->new_config.has_drive_strength_ua()) {
          FDF_LOG(WARNING, "Drive strength not returned for %u", pin);
          return ZX_ERR_BAD_STATE;
        }
        if (result->value()->new_config.drive_strength_ua() != driver_strength_ua.value()) {
          FDF_LOG(WARNING, "Actual drive strength (%lu) doesn't match expected (%lu) for %u",
                  result->value()->new_config.drive_strength_ua(), driver_strength_ua.value(), pin);
          return ZX_ERR_BAD_STATE;
        }
      }

      const auto& drive_type = config.drive_type();
      if (drive_type.has_value()) {
        if (!result->value()->new_config.has_drive_type()) {
          FDF_LOG(WARNING, "Drive type not returned for %u", pin);
          return ZX_ERR_BAD_STATE;
        }
        if (result->value()->new_config.drive_type() != drive_type.value()) {
          FDF_LOG(WARNING, "Actual drive type (%u) doesn't match expected (%u) for %u",
                  static_cast<uint32_t>(result->value()->new_config.drive_type()),
                  static_cast<uint32_t>(drive_type.value()), pin);
          return ZX_ERR_BAD_STATE;
        }
      }

      const auto& power_source = config.power_source();
      if (power_source.has_value()) {
        if (!result->value()->new_config.has_power_source()) {
          FDF_LOG(WARNING, "Power source not returned for %u", pin);
          return ZX_ERR_BAD_STATE;
        }
        if (result->value()->new_config.power_source() != power_source.value()) {
          FDF_LOG(WARNING, "Actual power source (%lu) doesn't match expected (%lu) for %u",
                  result->value()->new_config.power_source(), power_source.value(), pin);
          return ZX_ERR_BAD_STATE;
        }
      }
    } else if (call.Which() == fuchsia_hardware_pinimpl::InitCall::Tag::kBufferMode) {
      auto result = pinimpl.sync().buffer(arena)->SetBufferMode(
          pin, fidl::ToWire(arena, call.buffer_mode().value()));
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
