// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/audio_core/plug_detector.h"

#include <fcntl.h>
#include <fuchsia/hardware/audio/cpp/fidl.h>
#include <lib/fdio/directory.h>
#include <lib/fit/defer.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/trace/event.h>
#include <lib/zx/channel.h>
#include <zircon/compiler.h>

#include <memory>
#include <vector>

#include <fbl/unique_fd.h>

#include "src/lib/fsl/io/device_watcher.h"
#include "src/media/audio/audio_core/reporter.h"

namespace media::audio {
namespace {

static const struct {
  const char* path;
  bool is_input;
} AUDIO_DEVNODES[] = {
    {.path = "/dev/class/audio-output", .is_input = false},
    {.path = "/dev/class/audio-input", .is_input = true},
};

class PlugDetectorImpl : public PlugDetector {
 public:
  zx_status_t Start(Observer observer) final {
    TRACE_DURATION("audio", "PlugDetectorImpl::Start");
    // Start should only be called once.
    FX_DCHECK(watchers_.empty());
    FX_DCHECK(!observer_);
    FX_DCHECK(observer);

    observer_ = std::move(observer);

    // If we fail to set up monitoring for any of our target directories,
    // automatically stop monitoring all sources of device nodes.
    auto error_cleanup = fit::defer([this]() { Stop(); });

    // Create our watchers.
    for (const auto& devnode : AUDIO_DEVNODES) {
      auto watcher = fsl::DeviceWatcher::Create(
          devnode.path,
          [this, is_input = devnode.is_input](const fidl::ClientEnd<fuchsia_io::Directory>& dir,
                                              const std::string& filename) {
            AddAudioDevice(dir, filename, is_input);
          });

      if (watcher != nullptr) {
        watchers_.emplace_back(std::move(watcher));
      } else {
        FX_LOGS(ERROR) << "PlugDetectorImpl failed to create DeviceWatcher for '" << devnode.path
                       << "'.";
      }
    }

    error_cleanup.cancel();

    return ZX_OK;
  }

  void Stop() final {
    TRACE_DURATION("audio", "PlugDetectorImpl::Stop");
    observer_ = nullptr;
    watchers_.clear();
  }

 private:
  void AddAudioDevice(const fidl::ClientEnd<fuchsia_io::Directory>& dir, const std::string& name,
                      bool is_input) {
    TRACE_DURATION("audio", "PlugDetectorImpl::AddAudioDevice");
    if (!observer_) {
      return;
    }

    fuchsia::hardware::audio::StreamConfigConnectorPtr device;
    if (zx_status_t status = fdio_service_connect_at(dir.channel().get(), name.c_str(),
                                                     device.NewRequest().TakeChannel().release());
        status != ZX_OK) {
      Reporter::Singleton().FailedToConnectToDevice(name, is_input, status);
      FX_PLOGS(ERROR, status) << "Failed to connect to audio " << (is_input ? "input" : "output")
                              << " device '" << name << "'";

      return;
    }
    device.set_error_handler([name, is_input](zx_status_t res) {
      Reporter::Singleton().FailedToObtainStreamChannel(name, is_input, res);
      FX_PLOGS(ERROR, res) << "Failed to open channel to audio " << (is_input ? "input" : "output")
                           << " '" << name << "'";
    });
    fidl::InterfaceHandle<fuchsia::hardware::audio::StreamConfig> stream_config_client;
    fidl::InterfaceRequest<fuchsia::hardware::audio::StreamConfig> stream_config_server =
        stream_config_client.NewRequest();
    device->Connect(std::move(stream_config_server));
    observer_(name, is_input, std::move(stream_config_client));
  }
  Observer observer_;
  std::vector<std::unique_ptr<fsl::DeviceWatcher>> watchers_;
};

}  // namespace

std::unique_ptr<PlugDetector> PlugDetector::Create() {
  return std::make_unique<PlugDetectorImpl>();
}

}  // namespace media::audio
