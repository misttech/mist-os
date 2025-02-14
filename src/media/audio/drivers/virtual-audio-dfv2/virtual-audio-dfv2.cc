// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/drivers/virtual-audio-dfv2/virtual-audio-dfv2.h"

#include <lib/driver/component/cpp/driver_export.h>

#include "src/media/audio/drivers/virtual-audio-dfv2/virtual-audio-composite.h"

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
      FDF_LOG(ERROR, "Failed to get default configuration: Device type %u not supported",
              static_cast<uint32_t>(request->type));
      completer.ReplyError(fuchsia_virtualaudio::Error::kNotSupported);
      return;
  }
}

void VirtualAudio::AddDevice(AddDeviceRequestView request, AddDeviceCompleter::Sync& completer) {
  // TODO(b/388880875): Implement logic.
  completer.ReplyError(fuchsia_virtualaudio::Error::kNotSupported);
}

void VirtualAudio::GetNumDevices(GetNumDevicesCompleter::Sync& completer) {
  uint32_t num_inputs = 0;
  uint32_t num_outputs = 0;
  uint32_t num_unspecified_direction = 0;
  for (const auto& [_, device] : devices_) {
    std::optional is_input = device->is_input();
    if (is_input.has_value()) {
      if (is_input.value()) {
        num_inputs++;
      } else {
        num_outputs++;
      }
    } else {
      num_unspecified_direction++;
    }
  }
  completer.Reply(num_inputs, num_outputs, num_unspecified_direction);
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
