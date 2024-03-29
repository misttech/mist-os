// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_DEVICE_DETECTOR_H_
#define SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_DEVICE_DETECTOR_H_

#include <fidl/fuchsia.audio.device/cpp/common_types.h>
#include <fidl/fuchsia.audio.device/cpp/natural_types.h>
#include <fidl/fuchsia.hardware.audio/cpp/fidl.h>
#include <lib/fit/function.h>
#include <lib/zx/result.h>

#include <functional>
#include <memory>
#include <string>
#include <string_view>

#include "src/lib/fsl/io/device_watcher.h"

namespace media_audio {

// If true, we detect Composite devices directly (without CompositeConnector).
// If false, we detect Composite devices via the "trampoline" CompositeConnector.
// TODO(https://fxbug.dev/304551042): Convert VirtualAudioComposite to DFv2 and remove this flag.
constexpr bool kDetectDFv2CompositeDevices = true;

using DeviceDetectionHandler = std::function<void(
    std::string_view, fuchsia_audio_device::DeviceType, fuchsia_audio_device::DriverClient)>;

// This class detects devices and invokes the provided handler for those devices. It uses multiple
// file-system watchers that focus on the device file system (devfs), specifically the locations
// where registered audio devices are exposed (dev/class/audio-input, etc).
class DeviceDetector {
 public:
  // Immediately kick off watchers in 'devfs' directories where audio devices are found.
  // Upon detection, our DeviceDetectionHandler is run on the dispatcher's thread.
  static zx::result<std::shared_ptr<DeviceDetector>> Create(DeviceDetectionHandler handler,
                                                            async_dispatcher_t* dispatcher);
  virtual ~DeviceDetector() = default;

 private:
  DeviceDetector(DeviceDetectionHandler handler, async_dispatcher_t* dispatcher)
      : handler_(std::move(handler)), dispatcher_(dispatcher) {}
  DeviceDetector() = delete;

  zx_status_t StartDeviceWatchers();

  // Open a devnode at the given path; use its FDIO device channel to connect (retrieve) the
  // device's primary protocol (StreamConfig, etc).
  void DriverClientFromDevFs(const fidl::ClientEnd<fuchsia_io::Directory>& dir,
                             const std::string& name, fuchsia_audio_device::DeviceType device_type);

  DeviceDetectionHandler handler_;
  std::vector<std::unique_ptr<fsl::DeviceWatcher>> watchers_;

  async_dispatcher_t* dispatcher_;
};

}  // namespace media_audio

#endif  // SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_DEVICE_DETECTOR_H_
