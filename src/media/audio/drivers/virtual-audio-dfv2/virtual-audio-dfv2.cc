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
  // TODO(b/388880875): Implement logic.
  completer.ReplyError(fuchsia_virtualaudio::Error::kNotSupported);
}

void VirtualAudio::AddDevice(AddDeviceRequestView request, AddDeviceCompleter::Sync& completer) {
  // TODO(b/388880875): Implement logic.
  completer.ReplyError(fuchsia_virtualaudio::Error::kNotSupported);
}

void VirtualAudio::GetNumDevices(GetNumDevicesCompleter::Sync& completer) {
  // TODO(b/388880875): Implement logic.
  completer.Reply(0, 0, 0);
}

void VirtualAudio::RemoveAll(RemoveAllCompleter::Sync& completer) {
  // TODO(b/388880875): Implement logic.
  completer.Reply();
}

void VirtualAudio::Serve(fidl::ServerEnd<fuchsia_virtualaudio::Control> server) {
  bindings_.AddBinding(dispatcher(), std::move(server), this, fidl::kIgnoreBindingClosure);
}

}  // namespace virtual_audio

FUCHSIA_DRIVER_EXPORT(virtual_audio::VirtualAudio);
