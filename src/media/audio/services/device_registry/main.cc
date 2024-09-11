// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/syslog/cpp/log_settings.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/trace-provider/provider.h>

#include <memory>
#include <string>

#include "src/media/audio/services/common/fidl_thread.h"
#include "src/media/audio/services/device_registry/audio_device_registry.h"
#include "src/media/audio/services/device_registry/inspector.h"
#include "src/media/audio/services/device_registry/logging.h"

int main(int argc, const char** argv) {
  fuchsia_logging::LogSettingsBuilder builder;
  builder.WithTags({"audio_device_registry"}).BuildAndInitialize();
  ADR_LOG(media_audio::kLogMain) << "AudioDeviceRegistry is starting up";

  // Create a loop, and use it to create our AudioDeviceRegistry singleton...
  auto loop = std::make_shared<async::Loop>(&kAsyncLoopConfigAttachToCurrentThread);
  auto adr_thread = media_audio::FidlThread::CreateFromCurrentThread("AudioDeviceRegistryMain",
                                                                     loop->dispatcher());
  auto adr_service = std::make_shared<media_audio::AudioDeviceRegistry>(adr_thread);

  trace::TraceProviderWithFdio trace_provider(loop->dispatcher(), "audio_device_registry_provider");

  // ...then create the connection to Inspect, so we can chronicle the subsequent actions...
  media_audio::Inspector::Initialize(loop->dispatcher());

  // ...then start the device detection process (which continues after this call returns)...
  if (auto status = adr_service->StartDeviceDetection(); status != ZX_OK) {
    auto str = std::string("StartDeviceDetection failed to start devfs device detection: ") +
               std::to_string(status);
    FX_LOGS(ERROR) << str;
    media_audio::Inspector::Singleton()->RecordUnhealthy(str);
    return -1;
  }

  // ...then register the FIDL services and serve them out, so clients can call them...
  if (auto status = adr_service->RegisterAndServeOutgoing(); status != ZX_OK) {
    auto str = std::string("RegisterAndServeOutgoing failed to serve outgoing directory: ") +
               std::to_string(status);
    FX_LOGS(ERROR) << str;
    media_audio::Inspector::Singleton()->RecordUnhealthy(str);
    return -2;
  }

  // ...then chronicle that adr_service has completed its "starting up" steps...
  media_audio::Inspector::Singleton()->RecordHealthOk();

  // ...then run our loop here in main(), so AudioDeviceRegistry doesn't have to deal with it.
  loop->Run();

  ADR_LOG(media_audio::kLogMain) << "Exiting AudioDeviceRegistry main()";
  return 0;
}
