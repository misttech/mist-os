// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "platform-interrupt.h"

#include <fidl/fuchsia.hardware.interrupt/cpp/markers.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/ddk/binding_priv.h>
#include <lib/ddk/device.h>
#include <lib/ddk/platform-defs.h>

#include "src/devices/bus/drivers/platform/platform-device.h"

namespace platform_bus {

void PlatformInterruptFragment::Get(GetCompleter::Sync& completer) {
  zx::result result = pdev_->GetInterrupt(index_, 0);
  if (result.is_error()) {
    completer.ReplyError(result.status_value());
  } else {
    completer.ReplySuccess(std::move(*result));
  }
}

zx_status_t PlatformInterruptFragment::Add(const char* name, PlatformDevice* pdev,
                                           fuchsia_hardware_platform_bus::Irq& irq) {
  fuchsia_hardware_interrupt::Service::InstanceHandler handler(
      {.provider = bindings_.CreateHandler(this, dispatcher_, fidl::kIgnoreBindingClosure)});

  auto result = outgoing_.AddService<fuchsia_hardware_interrupt::Service>(std::move(handler));
  if (result.is_error()) {
    return result.status_value();
  }

  auto endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
  if (endpoints.is_error()) {
    return endpoints.status_value();
  }

  result = outgoing_.Serve(std::move(endpoints->server));
  if (result.is_error()) {
    return result.status_value();
  }

  std::array offers = {
      fuchsia_hardware_interrupt::Service::Name,
  };

  std::vector<zx_device_prop_t> props;
  std::vector<zx_device_str_prop_t> str_props;

  if (irq.properties().has_value()) {
    for (auto& prop : irq.properties().value()) {
      switch (prop.key().Which()) {
        case fuchsia_driver_framework::NodePropertyKey::Tag::kIntValue:
          if (prop.value().Which() != fuchsia_driver_framework::NodePropertyValue::Tag::kIntValue) {
            return ZX_ERR_NOT_SUPPORTED;
          }
          props.emplace_back(zx_device_prop_t{
              .id = static_cast<uint16_t>(prop.key().int_value().value()),
              .value = prop.value().int_value().value(),
          });
          break;

        case fuchsia_driver_framework::NodePropertyKey::Tag::kStringValue: {
          auto& str_prop = str_props.emplace_back(zx_device_str_prop_t{
              .key = prop.key().string_value()->data(),
          });
          switch (prop.value().Which()) {
            case fuchsia_driver_framework::NodePropertyValue::Tag::kStringValue:
              str_prop.property_value = str_prop_str_val(prop.value().string_value()->data());
              break;
            case fuchsia_driver_framework::NodePropertyValue::Tag::kIntValue:
              str_prop.property_value = str_prop_int_val(prop.value().int_value().value());
              break;
            case fuchsia_driver_framework::NodePropertyValue::Tag::kEnumValue:
              str_prop.property_value = str_prop_enum_val(prop.value().enum_value()->data());
              break;
            case fuchsia_driver_framework::NodePropertyValue::Tag::kBoolValue:
              str_prop.property_value = str_prop_bool_val(prop.value().bool_value().value());
              break;
            default:
              zxlogf(ERROR, "Invalid property value.");
              return ZX_ERR_INVALID_ARGS;
          }
          break;
        }
        default:
          zxlogf(ERROR, "Invalid key value.");
          return ZX_ERR_INVALID_ARGS;
      }
    }
  } else {
    props = {
        zx_device_prop_t{
            .id = BIND_PLATFORM_DEV_VID,
            .value = pdev->vid(),
        },
        zx_device_prop_t{
            .id = BIND_PLATFORM_DEV_DID,
            .value = pdev->did(),
        },
        zx_device_prop_t{
            .id = BIND_PLATFORM_DEV_PID,
            .value = pdev->pid(),
        },
        zx_device_prop_t{
            .id = BIND_PLATFORM_DEV_INSTANCE_ID,
            .value = pdev->instance_id(),
        },
        // Because "x == 0" is true if "x" is unset, start at 1.
        zx_device_prop_t{
            .id = BIND_PLATFORM_DEV_INTERRUPT_ID,
            .value = index_ + 1,
        },
    };
  }

  return DdkAdd(ddk::DeviceAddArgs(name)
                    .set_flags(DEVICE_ADD_MUST_ISOLATE)
                    .set_fidl_service_offers(offers)
                    .set_outgoing_dir(endpoints->client.TakeChannel())
                    .set_props(props)
                    .set_str_props(str_props));
}

}  // namespace platform_bus
