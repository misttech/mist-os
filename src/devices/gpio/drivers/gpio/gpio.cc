// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "gpio.h"

#include <fidl/fuchsia.scheduler/cpp/wire.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/metadata.h>
#include <lib/fit/defer.h>
#include <zircon/types.h>

#include <memory>

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
  gpio_->buffer(arena)
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
  gpio_->buffer(arena)
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
  gpio_->buffer(arena)->Read(pin_).ThenExactlyOnce(
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
  gpio_->buffer(arena)
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
  gpio_->buffer(arena)
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
  gpio_->buffer(arena)->GetDriveStrength(pin_).ThenExactlyOnce(
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
  gpio_->buffer(arena)
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
  gpio_->buffer(arena)->ReleaseInterrupt(pin_).ThenExactlyOnce(
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
  gpio_->buffer(arena)
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
  gpio_->buffer(arena)
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

zx_status_t GpioDevice::InitAddDevice(const uint32_t controller_id) {
  char name[20];
  snprintf(name, sizeof(name), "gpio-%u", pin_);

  zx_device_prop_t props[] = {
      {BIND_GPIO_PIN, 0, pin_},
      {BIND_GPIO_CONTROLLER, 0, controller_id},
  };

  auto endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
  if (endpoints.is_error()) {
    return endpoints.status_value();
  }

  fuchsia_hardware_gpio::Service::InstanceHandler handler({
      .device = bindings_->CreateHandler(this, fidl_dispatcher_->async_dispatcher(),
                                         fidl::kIgnoreBindingClosure),
  });
  auto result = outgoing_->AddService<fuchsia_hardware_gpio::Service>(std::move(handler));
  if (result.is_error()) {
    zxlogf(ERROR, "Failed to add service to the outgoing directory");
    return result.status_value();
  }

  result = outgoing_->Serve(std::move(endpoints->server));
  if (result.is_error()) {
    zxlogf(ERROR, "Failed to add service to the outgoing directory");
    return result.status_value();
  }

  std::array offers = {
      fuchsia_hardware_gpio::Service::Name,
  };

  return DdkAdd(ddk::DeviceAddArgs(name)
                    .set_proto_id(ZX_PROTOCOL_GPIO)
                    .set_props(props)
                    .set_fidl_service_offers(offers)
                    .set_outgoing_dir(endpoints->client.TakeChannel()));
}

void GpioDevice::DdkUnbind(ddk::UnbindTxn txn) {
  async::PostTask(fidl_dispatcher_->async_dispatcher(), [this, txn = std::move(txn)]() mutable {
    // The release hook must run synchronously on the main driver dispatcher, so destroy any
    // objects that live on the FIDL dispatcher during unbind instead.

    outgoing_.reset();
    bindings_.reset();
    gpio_.reset();

    txn.Reply();
  });
}

void GpioDevice::DdkRelease() { delete this; }

zx_status_t GpioRootDevice::Create(void* ctx, zx_device_t* parent) {
  uint32_t controller_id = 0;
  {
    zx::result gpio_fidl_client =
        DdkConnectRuntimeProtocol<fuchsia_hardware_gpioimpl::Service::Device>(parent);
    if (gpio_fidl_client.is_error()) {
      zxlogf(ERROR, "Failed to get gpioimpl protocol");
      return gpio_fidl_client.status_value();
    }

    fdf::WireSyncClient<fuchsia_hardware_gpioimpl::GpioImpl> gpio_fidl(
        *std::move(gpio_fidl_client));

    fdf::Arena arena('GPIO');
    if (const auto result = gpio_fidl.buffer(arena)->GetControllerId(); result.ok()) {
      controller_id = result->controller_id;
    } else {
      zxlogf(ERROR, "Failed to get controller ID: %s", result.status_string());
      return result.status();
    }

    // Process init metadata while we are still the exclusive owner of the GPIO client.
    GpioInitDevice::Create(parent, gpio_fidl.TakeClientEnd(), controller_id);
  }

  fbl::AllocChecker ac;
  std::unique_ptr<GpioRootDevice> root(new (&ac) GpioRootDevice(parent));
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  auto scheduler_role = ddk::GetEncodedMetadata<fuchsia_scheduler::wire::RoleName>(
      parent, DEVICE_METADATA_SCHEDULER_ROLE_NAME);
  if (scheduler_role.is_ok()) {
    const std::string role_name(scheduler_role->role.get());

    zx::result result = fdf::SynchronizedDispatcher::Create(
        {}, "GPIO", fit::bind_member<&GpioRootDevice::DispatcherShutdownHandler>(root.get()),
        role_name);
    if (result.is_error()) {
      zxlogf(ERROR, "Failed to create SynchronizedDispatcher: %s", result.status_string());
      return result.error_value();
    }

    // If scheduler role metadata was provided, create a new dispatcher using the role, and use
    // that dispatcher instead of the default dispatcher passed to this method.
    root->fidl_dispatcher_.emplace(*std::move(result));

    zxlogf(DEBUG, "Using dispatcher with role \"%s\"", role_name.c_str());
  }

  zx_status_t status = root->DdkAdd(ddk::DeviceAddArgs("gpio").set_flags(DEVICE_ADD_NON_BINDABLE));
  if (status != ZX_OK) {
    zxlogf(ERROR, "DdkAdd failed: %s", zx_status_get_string(status));
    return status;
  }

  auto root_release = fit::defer([&root]() { [[maybe_unused]] auto ptr = root.release(); });

  auto pins = ddk::GetMetadataArray<gpio_pin_t>(parent, DEVICE_METADATA_GPIO_PINS);
  if (pins.is_error()) {
    if (pins.status_value() == ZX_ERR_NOT_FOUND) {
      zxlogf(INFO, "No pins metadata provided");
    } else {
      zxlogf(ERROR, "Failed to get metadata array: %s", pins.status_string());
      return pins.status_value();
    }
  } else {
    // Make sure that the list of GPIO pins has no duplicates.
    auto gpio_cmp_lt = [](gpio_pin_t& lhs, gpio_pin_t& rhs) { return lhs.pin < rhs.pin; };
    auto gpio_cmp_eq = [](gpio_pin_t& lhs, gpio_pin_t& rhs) { return lhs.pin == rhs.pin; };
    std::sort(pins.value().begin(), pins.value().end(), gpio_cmp_lt);
    auto result = std::adjacent_find(pins.value().begin(), pins.value().end(), gpio_cmp_eq);
    if (result != pins.value().end()) {
      zxlogf(ERROR, "gpio pin '%d' was published more than once", result->pin);
      return ZX_ERR_INVALID_ARGS;
    }

    if (root->fidl_dispatcher_) {
      // Pin devices must be created on the FIDL dispatcher if it exists.
      async::PostTask(root->fidl_dispatcher_->async_dispatcher(),
                      [=, root = root.get(), pins = *std::move(pins)]() {
                        root->AddPinDevices(controller_id, pins);
                      });
    } else {
      return root->AddPinDevices(controller_id, *pins);
    }
  }

  return ZX_OK;
}

void GpioRootDevice::DdkUnbind(ddk::UnbindTxn txn) {
  ZX_ASSERT_MSG(!unbind_txn_, "unbind_txn_ already set");
  if (fidl_dispatcher_) {
    unbind_txn_.emplace(std::move(txn));

    // Post a task to the FIDL dispatcher to start shutting itself down. If run on this thread
    // instead, the dispatcher could change states while a task on it is running, potentially
    // causing an assert failure if that task was trying to bind a FIDL server.
    async::PostTask(fidl_dispatcher_->async_dispatcher(),
                    [&]() { fidl_dispatcher_->ShutdownAsync(); });
  } else {
    txn.Reply();
  }
}

void GpioRootDevice::DdkRelease() { delete this; }

void GpioRootDevice::DispatcherShutdownHandler(fdf_dispatcher_t* dispatcher) {
  async::PostTask(driver_dispatcher_->async_dispatcher(), [this]() {
    ZX_DEBUG_ASSERT_MSG(unbind_txn_,
                        "unbind_txn_ was not set before the FIDL dispatcher shut down");
    unbind_txn_->Reply();
  });
}

zx_status_t GpioRootDevice::AddPinDevices(const uint32_t controller_id,
                                          const std::vector<gpio_pin_t>& pins) {
  const fdf::UnownedDispatcher fidl_dispatcher =
      fidl_dispatcher_ ? fdf::UnownedDispatcher(fidl_dispatcher_->get())
                       : fdf::Dispatcher::GetCurrent();

  for (const auto& pin : pins) {
    zx::result gpio =
        DdkConnectRuntimeProtocol<fuchsia_hardware_gpioimpl::Service::Device>(parent());
    ZX_ASSERT_MSG(gpio.is_ok(), "Failed to get additional FIDL client: %s", gpio.status_string());

    fbl::AllocChecker ac;
    std::unique_ptr<GpioDevice> dev(new (&ac) GpioDevice(zxdev(), fidl_dispatcher->borrow(),
                                                         *std::move(gpio), pin.pin, pin.name));
    if (!ac.check()) {
      return ZX_ERR_NO_MEMORY;
    }

    zx_status_t status = dev->InitAddDevice(controller_id);
    if (status != ZX_OK) {
      zxlogf(ERROR, "Failed to add device: %s", zx_status_get_string(status));
      return status;
    }

    // dev is now owned by devmgr.
    [[maybe_unused]] auto ptr = dev.release();
  }

  return ZX_OK;
}

void GpioInitDevice::Create(zx_device_t* parent,
                            fdf::ClientEnd<fuchsia_hardware_gpioimpl::GpioImpl> gpio,
                            const uint32_t controller_id) {
  // Don't add the init device if anything goes wrong here, as the hardware may be in a state that
  // child devices don't expect.
  auto decoded = ddk::GetEncodedMetadata<fuchsia_hardware_gpioimpl::wire::InitMetadata>(
      parent, DEVICE_METADATA_GPIO_INIT);
  if (!decoded.is_ok()) {
    if (decoded.status_value() == ZX_ERR_NOT_FOUND) {
      zxlogf(INFO, "No init metadata provided");
    } else {
      zxlogf(ERROR, "Failed to decode metadata: %s", decoded.status_string());
    }
    return;
  }

  auto device = std::make_unique<GpioInitDevice>(parent);
  if (device->ConfigureGpios(*decoded.value(), fdf::WireSyncClient(std::move(gpio))) != ZX_OK) {
    // Return without adding the init device if some GPIOs could not be configured. This will
    // prevent all drivers that depend on the initial state from binding, which should make it more
    // obvious that something has gone wrong.
    return;
  }

  zx_device_prop_t props[] = {
      {BIND_INIT_STEP, 0, bind_fuchsia_gpio::BIND_INIT_STEP_GPIO},
      {BIND_GPIO_CONTROLLER, 0, controller_id},
  };

  zx_status_t status = device->DdkAdd(
      ddk::DeviceAddArgs("gpio-init").set_flags(DEVICE_ADD_ALLOW_MULTI_COMPOSITE).set_props(props));
  if (status == ZX_OK) {
    [[maybe_unused]] auto _ = device.release();
  } else {
    zxlogf(ERROR, "Failed to add gpio-init: %s", zx_status_get_string(status));
  }
}

zx_status_t GpioInitDevice::ConfigureGpios(
    const fuchsia_hardware_gpioimpl::wire::InitMetadata& metadata,
    fdf::WireSyncClient<fuchsia_hardware_gpioimpl::GpioImpl> gpio) {
  // Stop processing the list if any call returns an error so that GPIOs are not accidentally put
  // into an unexpected state.
  for (const auto& step : metadata.steps) {
    fdf::Arena arena('GPIO');

    if (step.call.is_input_flags()) {
      auto result = gpio.buffer(arena)->ConfigIn(step.index, step.call.input_flags());
      if (!result.ok()) {
        zxlogf(ERROR, "Call to ConfigIn failed: %s", result.status_string());
        return result.status();
      }
      if (result->is_error()) {
        zxlogf(ERROR, "ConfigIn(%u) failed for %u: %s",
               static_cast<uint32_t>(step.call.input_flags()), step.index,
               zx_status_get_string(result->error_value()));
        return result->error_value();
      }
    } else if (step.call.is_output_value()) {
      auto result = gpio.buffer(arena)->ConfigOut(step.index, step.call.output_value());
      if (!result.ok()) {
        zxlogf(ERROR, "Call to ConfigOut failed: %s", result.status_string());
        return result.status();
      }
      if (result->is_error()) {
        zxlogf(ERROR, "ConfigOut(%u) failed for %u: %s", step.call.output_value(), step.index,
               zx_status_get_string(result->error_value()));
        return result->error_value();
      }
    } else if (step.call.is_alt_function()) {
      auto result = gpio.buffer(arena)->SetAltFunction(step.index, step.call.alt_function());
      if (!result.ok()) {
        zxlogf(ERROR, "Call to SetAltFunction failed: %s", result.status_string());
        return result.status();
      }
      if (result->is_error()) {
        zxlogf(ERROR, "SetAltFunction(%lu) failed for %u: %s", step.call.alt_function(), step.index,
               zx_status_get_string(result->error_value()));
        return result->error_value();
      }
    } else if (step.call.is_drive_strength_ua()) {
      auto result = gpio.buffer(arena)->SetDriveStrength(step.index, step.call.drive_strength_ua());
      if (!result.ok()) {
        zxlogf(ERROR, "Call to SetDriveStrength failed: %s", result.status_string());
        return result.status();
      }
      if (result->is_error()) {
        zxlogf(ERROR, "SetDriveStrength(%lu) failed for %u: %s", step.call.drive_strength_ua(),
               step.index, zx_status_get_string(result->error_value()));
        return result->error_value();
      }
      if (result->value()->actual_ds_ua != step.call.drive_strength_ua()) {
        zxlogf(WARNING, "Actual drive strength (%lu) doesn't match expected (%lu) for %u",
               result->value()->actual_ds_ua, step.call.drive_strength_ua(), step.index);
        return ZX_ERR_BAD_STATE;
      }
    } else if (step.call.is_delay()) {
      zx::nanosleep(zx::deadline_after(zx::duration(step.call.delay())));
    }
  }

  return ZX_OK;
}

static constexpr zx_driver_ops_t driver_ops = []() {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = GpioRootDevice::Create;
  return ops;
}();

}  // namespace gpio

ZIRCON_DRIVER(gpio, gpio::driver_ops, "zircon", "0.1");
