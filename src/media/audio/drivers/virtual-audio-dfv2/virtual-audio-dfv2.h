// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_DRIVERS_VIRTUAL_AUDIO_DFV2_VIRTUAL_AUDIO_DFV2_H_
#define SRC_MEDIA_AUDIO_DRIVERS_VIRTUAL_AUDIO_DFV2_VIRTUAL_AUDIO_DFV2_H_

#include <fidl/fuchsia.virtualaudio/cpp/fidl.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/devfs/cpp/connector.h>

namespace virtual_audio {

class VirtualAudio : public fdf::DriverBase,
                     public fidl::WireServer<fuchsia_virtualaudio::Control> {
 public:
  static constexpr std::string_view kDriverName = "virtual_audio";
  static constexpr std::string_view kChildNodeName = "virtual_audio";
  static constexpr std::string_view kClassName = "virtual_audio";

  VirtualAudio(fdf::DriverStartArgs start_args,
               fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : DriverBase(kDriverName, std::move(start_args), std::move(driver_dispatcher)) {}

  zx::result<> Start() override;

 private:
  // Implements virtualaudio.Control.
  void GetDefaultConfiguration(GetDefaultConfigurationRequestView request,
                               GetDefaultConfigurationCompleter::Sync& completer) override;
  void AddDevice(AddDeviceRequestView request, AddDeviceCompleter::Sync& completer) override;
  void GetNumDevices(GetNumDevicesCompleter::Sync& completer) override;
  void RemoveAll(RemoveAllCompleter::Sync& completer) override;

  void Serve(fidl::ServerEnd<fuchsia_virtualaudio::Control> server);

  fdf::OwnedChildNode child_;
  fidl::ServerBindingGroup<fuchsia_virtualaudio::Control> bindings_;
  driver_devfs::Connector<fuchsia_virtualaudio::Control> devfs_connector_{
      fit::bind_member<&VirtualAudio::Serve>(this)};
};

}  // namespace virtual_audio

#endif  // SRC_MEDIA_AUDIO_DRIVERS_VIRTUAL_AUDIO_DFV2_VIRTUAL_AUDIO_DFV2_H_
