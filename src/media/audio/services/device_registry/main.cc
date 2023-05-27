// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/syslog/cpp/macros.h>

#include <memory>

#include "src/media/audio/services/common/fidl_thread.h"
#include "src/media/audio/services/device_registry/audio_device_registry.h"

int main(int argc, const char** argv) {
  FX_LOGS(INFO) << "AudioDeviceRegistry is starting up";

  // Create a loop, and use it to create our AudioDeviceRegistry singleton...
  auto loop = std::make_shared<async::Loop>(&kAsyncLoopConfigAttachToCurrentThread);
  auto adr_thread = media_audio::FidlThread::CreateFromCurrentThread("AudioDeviceRegistryMain",
                                                                     loop->dispatcher());
  auto adr_service = std::make_shared<media_audio::AudioDeviceRegistry>(adr_thread);

  // ...then start the device detection process (which continues after this call returns)...
  if (adr_service->StartDeviceDetection() != ZX_OK) {
    return -1;
  }

  // ...then register the FIDL services and serve them out, so clients can call them...
  if (adr_service->RegisterAndServeOutgoing() != ZX_OK) {
    FX_LOGS(ERROR) << "RegisterAndServeOutgoing failed to serve outgoing directory";
    return -2;
  }

  // ...then run our loop here in main(), so AudioDeviceRegistry doesn't have to deal with it.
  loop->Run();

  FX_LOGS(INFO) << "Exiting AudioDeviceRegistry main()";
  return 0;
}
