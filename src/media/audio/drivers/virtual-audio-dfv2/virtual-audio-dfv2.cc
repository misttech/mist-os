// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/drivers/virtual-audio-dfv2/virtual-audio-dfv2.h"

#include <lib/driver/component/cpp/driver_export.h>

namespace virtual_audio {

zx::result<> VirtualAudio::Start() {
  zx::result connector = devfs_connector_.Bind(dispatcher());
  if (connector.is_error()) {
    FDF_LOG(ERROR, "Failed to bind devfs connector: %s", connector.status_string());
    return connector.take_error();
  }

  auto devfs_args = fuchsia_driver_framework::DevfsAddArgs{{
      .connector = std::move(connector.value()),
      .class_name{kClassName},
  }};

  zx::result child = AddOwnedChild(kChildNodeName, devfs_args);
  if (child.is_error()) {
    FDF_LOG(ERROR, "Failed to add child: %s", child.status_string());
    return child.take_error();
  }

  child_ = std::move(child.value());

  return zx::ok();
}

void VirtualAudio::GetDefaultConfiguration(GetDefaultConfigurationRequestView request,
                                           GetDefaultConfigurationCompleter::Sync& completer) {
  fidl::Arena arena;
  switch (request->type) {
    case fuchsia_virtualaudio::wire::DeviceType::kComposite:
      completer.ReplySuccess(fidl::ToWire(arena, VirtualAudioComposite::GetDefaultConfig()));
      return;
    default:
      FDF_LOG(ERROR, "Failed to get default configuration: Device type not supported");
      completer.ReplyError(fuchsia_virtualaudio::Error::kNotSupported);
      return;
  }
}

void VirtualAudio::AddDevice(AddDeviceRequestView request, AddDeviceCompleter::Sync& completer) {
  const auto& config = fidl::ToNatural(request->config);
  const std::optional device_specific = config.device_specific();
  if (!device_specific.has_value()) {
    FDF_LOG(ERROR, "Missing device_specific field");
    completer.ReplyError(fuchsia_virtualaudio::Error::kInvalidArgs);
    return;
  }
  const auto& device_type = device_specific.value().Which();
  if (device_type != fuchsia_virtualaudio::DeviceSpecific::Tag::kComposite) {
    FDF_LOG(ERROR, "Unsupported device type %u",
            static_cast<uint32_t>(device_specific.value().Which()));
    completer.ReplyError(fuchsia_virtualaudio::Error::kInvalidArgs);
    return;
  }

  auto device_instance_id = next_device_instance_id_++;
  zx::result device = VirtualAudioComposite::Create(
      device_instance_id, dispatcher(), std::move(request->server),
      [this, device_instance_id](auto _) {
        FDF_LOG(INFO, "Removing device %lu: Device's binding closed", device_instance_id);
        devices_.erase(device_instance_id);
      },
      [this](std::string_view child_node_name, fuchsia_driver_framework::DevfsAddArgs& devfs_args) {
        return AddOwnedChild(child_node_name, devfs_args);
      });
  if (device.is_error()) {
    FDF_LOG(ERROR, "Failed to create virtual audio composite device: %s", device.status_string());
    completer.ReplyError(fuchsia_virtualaudio::Error::kInternal);
    return;
  }
  devices_[device_instance_id] = std::move(device.value());
  FDF_LOG(INFO, "Create virtual audio composite device %lu", device_instance_id);

  completer.ReplySuccess();
}

void VirtualAudio::GetNumDevices(GetNumDevicesCompleter::Sync& completer) {
  completer.Reply(0, 0, static_cast<uint32_t>(devices_.size()));
}

void VirtualAudio::RemoveAll(RemoveAllCompleter::Sync& completer) {
  devices_.clear();
  completer.Reply();
}

void VirtualAudio::Serve(fidl::ServerEnd<fuchsia_virtualaudio::Control> server) {
  bindings_.AddBinding(dispatcher(), std::move(server), this, fidl::kIgnoreBindingClosure);
}

}  // namespace virtual_audio

FUCHSIA_DRIVER_EXPORT(virtual_audio::VirtualAudio);
