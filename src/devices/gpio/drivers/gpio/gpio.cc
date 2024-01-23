// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "gpio.h"

#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/device.h>
#include <lib/ddk/metadata.h>
#include <zircon/types.h>

#include <memory>

#include <bind/fuchsia/gpio/cpp/bind.h>
#include <ddk/metadata/gpio.h>
#include <fbl/alloc_checker.h>
#include <fbl/auto_lock.h>

namespace gpio {

zx_status_t GpioDevice::GpioConfigIn(uint32_t flags) {
  fbl::AutoLock lock(&lock_);
  return gpio_.ConfigIn(pin_, flags);
}

zx_status_t GpioDevice::GpioConfigOut(uint8_t initial_value) {
  fbl::AutoLock lock(&lock_);
  return gpio_.ConfigOut(pin_, initial_value);
}

zx_status_t GpioDevice::GpioSetAltFunction(uint64_t function) {
  fbl::AutoLock lock(&lock_);
  return gpio_.SetAltFunction(pin_, function);
}

zx_status_t GpioDevice::GpioRead(uint8_t* out_value) {
  fbl::AutoLock lock(&lock_);
  return gpio_.Read(pin_, out_value);
}

zx_status_t GpioDevice::GpioWrite(uint8_t value) {
  fbl::AutoLock lock(&lock_);
  return gpio_.Write(pin_, value);
}

zx_status_t GpioDevice::GpioGetInterrupt(uint32_t flags, zx::interrupt* out_irq) {
  fbl::AutoLock lock(&lock_);
  return gpio_.GetInterrupt(pin_, flags, out_irq);
}

zx_status_t GpioDevice::GpioReleaseInterrupt() {
  fbl::AutoLock lock(&lock_);
  return gpio_.ReleaseInterrupt(pin_);
}

zx_status_t GpioDevice::GpioSetPolarity(gpio_polarity_t polarity) {
  fbl::AutoLock lock(&lock_);
  return gpio_.SetPolarity(pin_, polarity);
}

zx_status_t GpioDevice::GpioGetDriveStrength(uint64_t* ds_ua) {
  fbl::AutoLock lock(&lock_);
  return gpio_.GetDriveStrength(pin_, ds_ua);
}

zx_status_t GpioDevice::GpioSetDriveStrength(uint64_t ds_ua, uint64_t* out_actual_ds_ua) {
  fbl::AutoLock lock(&lock_);
  return gpio_.SetDriveStrength(pin_, ds_ua, out_actual_ds_ua);
}

zx_status_t GpioDevice::InitAddDevice(const uint32_t controller_id) {
  char name[20];
  snprintf(name, sizeof(name), "gpio-%u", pin_);

  zx_device_prop_t props[] = {
      {BIND_GPIO_PIN, 0, pin_},
      {BIND_GPIO_CONTROLLER, 0, controller_id},
  };

  async_dispatcher_t* dispatcher =
      fdf_dispatcher_get_async_dispatcher(fdf_dispatcher_get_current_dispatcher());
  outgoing_ = component::OutgoingDirectory(dispatcher);

  auto endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
  if (endpoints.is_error()) {
    return endpoints.status_value();
  }

  fuchsia_hardware_gpio::Service::InstanceHandler handler({
      .device = bindings_.CreateHandler(this, dispatcher, fidl::kIgnoreBindingClosure),
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

  return DdkAdd(
      ddk::DeviceAddArgs(name).set_props(props).set_fidl_service_offers(offers).set_outgoing_dir(
          endpoints->client.TakeChannel()));
}

void GpioDevice::DdkUnbind(ddk::UnbindTxn txn) {
  if (binding_.has_value()) {
    Binding& binding = binding_.value();
    ZX_ASSERT(!binding.unbind_txn.has_value());
    binding.unbind_txn.emplace(std::move(txn));
    binding.binding.Unbind();
  } else {
    txn.Reply();
  }
}

void GpioDevice::DdkRelease() { delete this; }

zx_status_t GpioRootDevice::Create(void* ctx, zx_device_t* parent) {
  const ddk::GpioImplProtocolClient gpio_banjo(parent);
  if (gpio_banjo.is_valid()) {
    zxlogf(INFO, "Using Banjo gpioimpl protocol");
  }

  uint32_t controller_id = 0;
  {
    fdf::WireSyncClient<fuchsia_hardware_gpioimpl::GpioImpl> gpio_fidl;
    if (!gpio_banjo.is_valid()) {
      zx::result gpio_fidl_client =
          DdkConnectRuntimeProtocol<fuchsia_hardware_gpioimpl::Service::Device>(parent);
      if (gpio_fidl_client.is_ok()) {
        zxlogf(INFO, "Failed to get Banjo gpioimpl protocol, falling back to FIDL");
        gpio_fidl = fdf::WireSyncClient(std::move(*gpio_fidl_client));
      } else {
        zxlogf(ERROR, "Failed to get Banjo or FIDL gpioimpl protocol");
        return ZX_ERR_NO_RESOURCES;
      }

      fdf::Arena arena('GPIO');
      if (const auto result = gpio_fidl.buffer(arena)->GetControllerId(); result.ok()) {
        controller_id = result->controller_id;
      } else {
        zxlogf(ERROR, "Failed to get controller ID: %s", result.status_string());
        return result.status();
      }
    }

    // Process init metadata while we are still the exclusive owner of the GPIO client.
    GpioInitDevice::Create(parent, {gpio_banjo, std::move(gpio_fidl)}, controller_id);
  }

  fbl::AllocChecker ac;
  std::unique_ptr<GpioRootDevice> root(new (&ac) GpioRootDevice(parent));
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  zx_status_t status = root->DdkAdd(ddk::DeviceAddArgs("gpio").set_flags(DEVICE_ADD_NON_BINDABLE));
  if (status != ZX_OK) {
    zxlogf(ERROR, "DdkAdd failed: %s", zx_status_get_string(status));
    return status;
  }

  status = root->AddPinDevices(controller_id, gpio_banjo);

  [[maybe_unused]] auto ptr = root.release();

  return status;
}

void GpioRootDevice::DdkRelease() { delete this; }

zx_status_t GpioRootDevice::AddPinDevices(const uint32_t controller_id,
                                          const ddk::GpioImplProtocolClient& gpio_banjo) {
  auto pins = ddk::GetMetadataArray<gpio_pin_t>(parent(), DEVICE_METADATA_GPIO_PINS);
  if (!pins.is_ok()) {
    zxlogf(ERROR, "Failed to get metadata array: %s", pins.status_string());
    return pins.error_value();
  }

  // Make sure that the list of GPIO pins has no duplicates.
  auto gpio_cmp_lt = [](gpio_pin_t& lhs, gpio_pin_t& rhs) { return lhs.pin < rhs.pin; };
  auto gpio_cmp_eq = [](gpio_pin_t& lhs, gpio_pin_t& rhs) { return lhs.pin == rhs.pin; };
  std::sort(pins.value().begin(), pins.value().end(), gpio_cmp_lt);
  auto result = std::adjacent_find(pins.value().begin(), pins.value().end(), gpio_cmp_eq);
  if (result != pins.value().end()) {
    zxlogf(ERROR, "gpio pin '%d' was published more than once", result->pin);
    return ZX_ERR_INVALID_ARGS;
  }

  for (auto pin : *pins) {
    fdf::WireSyncClient<fuchsia_hardware_gpioimpl::GpioImpl> gpio_fidl;
    if (!gpio_banjo.is_valid()) {
      zx::result gpio_fidl_client =
          DdkConnectRuntimeProtocol<fuchsia_hardware_gpioimpl::Service::Device>(parent());
      ZX_ASSERT_MSG(gpio_fidl_client.is_ok(), "Failed to get additional FIDL client: %s",
                    gpio_fidl_client.status_string());
      gpio_fidl = fdf::WireSyncClient(std::move(*gpio_fidl_client));
    }

    GpioImplProxy gpio(gpio_banjo, std::move(gpio_fidl));

    fbl::AllocChecker ac;
    std::unique_ptr<GpioDevice> dev(new (&ac)
                                        GpioDevice(zxdev(), std::move(gpio), pin.pin, pin.name));
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

void GpioInitDevice::Create(zx_device_t* parent, GpioImplProxy gpio, const uint32_t controller_id) {
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
  if (device->ConfigureGpios(*decoded.value(), gpio) != ZX_OK) {
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
    const fuchsia_hardware_gpioimpl::wire::InitMetadata& metadata, const GpioImplProxy& gpio) {
  // Stop processing the list if any call returns an error so that GPIOs are not accidentally put
  // into an unexpected state.
  for (const auto& step : metadata.steps) {
    if (step.call.is_input_flags()) {
      if (zx_status_t status =
              gpio.ConfigIn(step.index, static_cast<uint32_t>(step.call.input_flags()));
          status != ZX_OK) {
        zxlogf(ERROR, "ConfigIn(%u) failed for %u: %s",
               static_cast<uint32_t>(step.call.input_flags()), step.index,
               zx_status_get_string(status));
        return status;
      }
    } else if (step.call.is_output_value()) {
      if (zx_status_t status = gpio.ConfigOut(step.index, step.call.output_value());
          status != ZX_OK) {
        zxlogf(ERROR, "ConfigOut(%u) failed for %u: %s", step.call.output_value(), step.index,
               zx_status_get_string(status));
        return status;
      }
    } else if (step.call.is_alt_function()) {
      if (zx_status_t status = gpio.SetAltFunction(step.index, step.call.alt_function());
          status != ZX_OK) {
        zxlogf(ERROR, "SetAltFunction(%lu) failed for %u: %s", step.call.drive_strength_ua(),
               step.index, zx_status_get_string(status));
        return status;
      }
    } else if (step.call.is_drive_strength_ua()) {
      uint64_t actual_ds;
      if (zx_status_t status =
              gpio.SetDriveStrength(step.index, step.call.drive_strength_ua(), &actual_ds);
          status != ZX_OK) {
        zxlogf(ERROR, "SetDriveStrength(%lu) failed for %u: %s", step.call.drive_strength_ua(),
               step.index, zx_status_get_string(status));
        return status;
      } else if (actual_ds != step.call.drive_strength_ua()) {
        zxlogf(WARNING, "Actual drive strength (%lu) doesn't match expected (%lu) for %u",
               actual_ds, step.call.drive_strength_ua(), step.index);
        return ZX_ERR_BAD_STATE;
      }
    } else if (step.call.is_delay()) {
      zx::nanosleep(zx::deadline_after(zx::duration(step.call.delay())));
    }
  }

  return ZX_OK;
}

zx_status_t GpioImplProxy::ConfigIn(uint32_t index, uint32_t flags) const {
  if (gpio_banjo_.is_valid()) {
    return gpio_banjo_.ConfigIn(index, flags);
  }

  fdf::Arena arena('GPIO');
  const auto result = gpio_fidl_.buffer(arena)->ConfigIn(
      index, static_cast<fuchsia_hardware_gpio::GpioFlags>(flags));
  if (!result.ok()) {
    return result.status();
  }

  return result->is_error() ? result->error_value() : ZX_OK;
}

zx_status_t GpioImplProxy::ConfigOut(uint32_t index, uint8_t initial_value) const {
  if (gpio_banjo_.is_valid()) {
    return gpio_banjo_.ConfigOut(index, initial_value);
  }

  fdf::Arena arena('GPIO');
  const auto result = gpio_fidl_.buffer(arena)->ConfigOut(index, initial_value);
  if (!result.ok()) {
    return result.status();
  }

  return result->is_error() ? result->error_value() : ZX_OK;
}

zx_status_t GpioImplProxy::SetAltFunction(uint32_t index, uint64_t function) const {
  if (gpio_banjo_.is_valid()) {
    return gpio_banjo_.SetAltFunction(index, function);
  }

  fdf::Arena arena('GPIO');
  const auto result = gpio_fidl_.buffer(arena)->SetAltFunction(index, function);
  if (!result.ok()) {
    return result.status();
  }

  return result->is_error() ? result->error_value() : ZX_OK;
}

zx_status_t GpioImplProxy::Read(uint32_t index, uint8_t* out_value) const {
  if (gpio_banjo_.is_valid()) {
    return gpio_banjo_.Read(index, out_value);
  }

  fdf::Arena arena('GPIO');
  const auto result = gpio_fidl_.buffer(arena)->Read(index);
  if (!result.ok()) {
    return result.status();
  }
  if (result->is_error()) {
    return result->error_value();
  }

  *out_value = result->value()->value;
  return ZX_OK;
}

zx_status_t GpioImplProxy::Write(uint32_t index, uint8_t value) const {
  if (gpio_banjo_.is_valid()) {
    return gpio_banjo_.Write(index, value);
  }

  fdf::Arena arena('GPIO');
  const auto result = gpio_fidl_.buffer(arena)->Write(index, value);
  if (!result.ok()) {
    return result.status();
  }

  return result->is_error() ? result->error_value() : ZX_OK;
}

zx_status_t GpioImplProxy::GetInterrupt(uint32_t index, uint32_t flags,
                                        zx::interrupt* out_irq) const {
  if (gpio_banjo_.is_valid()) {
    return gpio_banjo_.GetInterrupt(index, flags, out_irq);
  }

  fdf::Arena arena('GPIO');
  const auto result = gpio_fidl_.buffer(arena)->GetInterrupt(index, flags);
  if (!result.ok()) {
    return result.status();
  }
  if (result->is_error()) {
    return result->error_value();
  }

  *out_irq = std::move(result->value()->irq);
  return ZX_OK;
}

zx_status_t GpioImplProxy::ReleaseInterrupt(uint32_t index) const {
  if (gpio_banjo_.is_valid()) {
    return gpio_banjo_.ReleaseInterrupt(index);
  }

  fdf::Arena arena('GPIO');
  const auto result = gpio_fidl_.buffer(arena)->ReleaseInterrupt(index);
  if (!result.ok()) {
    return result.status();
  }

  return result->is_error() ? result->error_value() : ZX_OK;
}

zx_status_t GpioImplProxy::SetPolarity(uint32_t index, gpio_polarity_t polarity) const {
  if (gpio_banjo_.is_valid()) {
    return gpio_banjo_.SetPolarity(index, polarity);
  }

  fdf::Arena arena('GPIO');
  const auto result = gpio_fidl_.buffer(arena)->SetPolarity(
      index, static_cast<fuchsia_hardware_gpio::GpioPolarity>(polarity));
  if (!result.ok()) {
    return result.status();
  }

  return result->is_error() ? result->error_value() : ZX_OK;
}

zx_status_t GpioImplProxy::GetDriveStrength(uint32_t index, uint64_t* out_value) const {
  if (gpio_banjo_.is_valid()) {
    return gpio_banjo_.GetDriveStrength(index, out_value);
  }

  fdf::Arena arena('GPIO');
  const auto result = gpio_fidl_.buffer(arena)->GetDriveStrength(index);
  if (!result.ok()) {
    return result.status();
  }
  if (result->is_error()) {
    return result->error_value();
  }

  *out_value = result->value()->result_ua;
  return ZX_OK;
}

zx_status_t GpioImplProxy::SetDriveStrength(uint32_t index, uint64_t ds_ua,
                                            uint64_t* out_actual_ua) const {
  if (gpio_banjo_.is_valid()) {
    return gpio_banjo_.SetDriveStrength(index, ds_ua, out_actual_ua);
  }

  fdf::Arena arena('GPIO');
  const auto result = gpio_fidl_.buffer(arena)->SetDriveStrength(index, ds_ua);
  if (!result.ok()) {
    return result.status();
  }
  if (result->is_error()) {
    return result->error_value();
  }

  *out_actual_ua = result->value()->actual_ds_ua;
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
