// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "serial.h"

#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <zircon/status.h>
#include <zircon/threads.h>

#include <memory>

#include <bind/fuchsia/cpp/bind.h>
#include <bind/fuchsia/serial/cpp/bind.h>
#include <fbl/alloc_checker.h>

namespace serial {

void SerialDevice::Read(ReadCompleter::Sync& completer) {
  FDF_LOG(TRACE, "SerialDevice::Read");

  fdf::Arena arena('SERI');
  serial_.buffer(arena)->Read().Then([completer = completer.ToAsync()](auto& result) mutable {
    if (!result.ok()) {
      completer.ReplyError(result.status());
    } else if (result->is_error()) {
      completer.ReplyError(result->error_value());
    } else {
      completer.ReplySuccess(result->value()->data);
    }
  });
}

void SerialDevice::Write(WriteRequestView request, WriteCompleter::Sync& completer) {
  fdf::Arena arena('SERI');
  serial_.buffer(arena)
      ->Write(request->data)
      .Then([completer = completer.ToAsync()](auto& result) mutable {
        if (!result.ok()) {
          completer.ReplyError(result.status());
        } else if (result->is_error()) {
          completer.ReplyError(result->error_value());
        } else {
          completer.ReplySuccess();
        }
      });
}

void SerialDevice::GetChannel(GetChannelRequestView request, GetChannelCompleter::Sync& completer) {
  FDF_LOG(TRACE, "SerialDevice::GetChannel");
  if (zx_status_t status = Bind(std::move(request->req)); status != ZX_OK) {
    FDF_LOG(ERROR, "SerialDevice::GetChannel error: %s", zx_status_get_string(status));
    completer.Close(status);
  }
}

void SerialDevice::GetClass(GetClassCompleter::Sync& completer) {
  FDF_LOG(TRACE, "SerialDevice::GetClass");
  completer.Reply(static_cast<fuchsia_hardware_serial::wire::Class>(serial_class_));
}

void SerialDevice::SetConfig(SetConfigRequestView request, SetConfigCompleter::Sync& completer) {
  using fuchsia_hardware_serial::wire::CharacterWidth;
  using fuchsia_hardware_serial::wire::FlowControl;
  using fuchsia_hardware_serial::wire::Parity;
  using fuchsia_hardware_serial::wire::StopWidth;
  uint32_t flags = 0;
  switch (request->config.character_width) {
    case CharacterWidth::kBits5:
      flags |= fuchsia_hardware_serialimpl::wire::kSerialDataBits5;
      break;
    case CharacterWidth::kBits6:
      flags |= fuchsia_hardware_serialimpl::wire::kSerialDataBits6;
      break;
    case CharacterWidth::kBits7:
      flags |= fuchsia_hardware_serialimpl::wire::kSerialDataBits7;
      break;
    case CharacterWidth::kBits8:
      flags |= fuchsia_hardware_serialimpl::wire::kSerialDataBits8;
      break;
  }

  switch (request->config.stop_width) {
    case StopWidth::kBits1:
      flags |= fuchsia_hardware_serialimpl::wire::kSerialStopBits1;
      break;
    case StopWidth::kBits2:
      flags |= fuchsia_hardware_serialimpl::wire::kSerialStopBits2;
      break;
  }

  switch (request->config.parity) {
    case Parity::kNone:
      flags |= fuchsia_hardware_serialimpl::wire::kSerialParityNone;
      break;
    case Parity::kEven:
      flags |= fuchsia_hardware_serialimpl::wire::kSerialParityEven;
      break;
    case Parity::kOdd:
      flags |= fuchsia_hardware_serialimpl::wire::kSerialParityOdd;
      break;
  }

  switch (request->config.control_flow) {
    case FlowControl::kNone:
      flags |= fuchsia_hardware_serialimpl::wire::kSerialFlowCtrlNone;
      break;
    case FlowControl::kCtsRts:
      flags |= fuchsia_hardware_serialimpl::wire::kSerialFlowCtrlCtsRts;
      break;
  }

  fdf::Arena arena('SERI');
  serial_.buffer(arena)
      ->Config(request->config.baud_rate, flags)
      .Then([completer = completer.ToAsync()](auto& result) mutable {
        if (result.ok()) {
          completer.Reply(result->is_error() ? result->error_value() : ZX_OK);
        } else {
          completer.Reply(result.status());
        }
      });
}

zx_status_t SerialDevice::Enable(bool enable) {
  fdf::Arena arena('SERI');
  auto result = serial_.sync().buffer(arena)->Enable(enable);
  if (!result.ok()) {
    return result.status();
  }
  if (result->is_error()) {
    return result->error_value();
  }
  return ZX_OK;
}

zx_status_t SerialDevice::CancelAll() {
  fdf::Arena arena('SERI');
  return serial_.sync().buffer(arena)->CancelAll().status();
}

zx_status_t SerialDevice::Bind(fidl::ServerEnd<fuchsia_hardware_serial::Device> server) {
  if (binding_.has_value()) {
    FDF_LOG(WARNING, "SerialDevice::Bind - already bound!");
    return ZX_ERR_ALREADY_BOUND;
  }

  if (zx_status_t status = Enable(true); status != ZX_OK) {
    return status;
  }

  binding_.emplace(fdf::Dispatcher::GetCurrent()->async_dispatcher(), std::move(server), this,
                   [](SerialDevice* self, fidl::UnbindInfo) {
                     FDF_LOG(TRACE, "SerialDevice::Bind - on close");

                     // Clear any pending read or write requests and disable the device.
                     self->CancelAll();
                     self->Enable(false);
                     self->binding_.reset();
                   });
  return ZX_OK;
}

void SerialDevice::DevfsConnect(fidl::ServerEnd<fuchsia_hardware_serial::DeviceProxy> server) {
  FDF_LOG(TRACE, "SerialDevice::DevfsConnect");

  proxy_bindings_.AddBinding(fdf::Dispatcher::GetCurrent()->async_dispatcher(), std::move(server),
                             this, fidl::kIgnoreBindingClosure);
}

void SerialDevice::PrepareStop(fdf::PrepareStopCompleter completer) {
  FDF_LOG(TRACE, "SerialDevice::PrepareStop");

  fdf::Arena arena('SERI');
  serial_.buffer(arena)->CancelAll().Then([&serial = serial_, arena = std::move(arena),
                                           completer = std::move(completer)](auto&) mutable {
    // Explicitly ignoring the result of CancelAll.
    FDF_LOG(TRACE, "SerialDevice::PrepareStop - pending operations aborted");
    serial.buffer(arena)->Enable(false).Then([completer = std::move(completer)](auto&) mutable {
      FDF_LOG(TRACE, "SerialDevice::PrepareStop - disabled serial");
      // Explicitly ignoring the result of Enable.
      completer(zx::ok());
    });
  });
}

zx::result<> SerialDevice::Start() {
  FDF_LOG(TRACE, "SerialDevice::Start");

  zx::result serial_client = incoming()->Connect<fuchsia_hardware_serialimpl::Service::Device>();
  if (serial_client.is_error()) {
    FDF_LOG(ERROR, "Failed to get FIDL serial client: %s", serial_client.status_string());
    return serial_client.take_error();
  }

  serial_.Bind(*std::move(serial_client), fdf::Dispatcher::GetCurrent()->get());

  if (zx_status_t status = Init(); status != ZX_OK) {
    return zx::error(status);
  }

  if (zx_status_t status = Bind(); status != ZX_OK) {
    FDF_LOG(ERROR, "SerialDevice::Create: Bind failed %s", zx_status_get_string(status));
    return zx::error(status);
  }

  return zx::ok();
}

zx_status_t SerialDevice::Init() {
  zx_status_t status = ZX_OK;
  fdf::Arena arena('SERI');
  if (auto result = serial_.sync().buffer(arena)->GetInfo(); !result.ok()) {
    status = result.status();
  } else if (result->is_error()) {
    status = result->error_value();
  } else {
    serial_class_ = static_cast<uint8_t>(result->value()->info.serial_class);
  }

  if (status != ZX_OK) {
    FDF_LOG(ERROR, "SerialDevice::Init: SerialImpl::GetInfo failed %s",
            zx_status_get_string(status));
  }

  return status;
}

zx_status_t SerialDevice::Bind() {
  fuchsia_hardware_serial::Service::InstanceHandler handler({
      .device =
          [this](fidl::ServerEnd<fuchsia_hardware_serial::Device> server) {
            Bind(std::move(server));
          },
  });
  zx::result<> result =
      outgoing()->AddService<fuchsia_hardware_serial::Service>(std::move(handler));
  if (result.is_error()) {
    FDF_LOG(ERROR, "Failed to add service to the outgoing directory: %s", result.status_string());
    return result.error_value();
  }

  FDF_LOG(TRACE, "SerialDevice added service to the outgoing directory");

  std::vector<fuchsia_driver_framework::Offer> offers{
      fdf::MakeOffer2<fuchsia_hardware_serial::Service>()};

  std::vector<fuchsia_driver_framework::NodeProperty> props{
      fdf::MakeProperty(bind_fuchsia::PROTOCOL, bind_fuchsia_serial::BIND_PROTOCOL_DEVICE),
      fdf::MakeProperty(bind_fuchsia::SERIAL_CLASS, serial_class_),
  };

  zx::result<fidl::ClientEnd<fuchsia_device_fs::Connector>> connector =
      devfs_connector_.Bind(fdf::Dispatcher::GetCurrent()->async_dispatcher());
  if (connector.is_error()) {
    FDF_LOG(ERROR, "Failed to bind devfs connector: %s", connector.status_string());
    return connector.error_value();
  }

  FDF_LOG(TRACE, "SerialDevice bound devfs connector");

  fuchsia_driver_framework::DevfsAddArgs devfs{{
      .connector = *std::move(connector),
      .class_name = "serial",
      .connector_supports = fuchsia_device_fs::ConnectionType::kDevice,
  }};

  zx::result<fidl::ClientEnd<fuchsia_driver_framework::NodeController>> controller =
      fdf::AddChild(node(), logger(), name(), devfs, props, offers);
  if (controller.is_error()) {
    FDF_LOG(ERROR, "AddChild failed: %s", controller.status_string());
    return controller.error_value();
  }

  FDF_LOG(TRACE, "SerialDevice registered devfs node: %s", std::string(name()).c_str());

  controller_ = *std::move(controller);
  return ZX_OK;
}

}  // namespace serial

FUCHSIA_DRIVER_EXPORT(serial::SerialDevice);
