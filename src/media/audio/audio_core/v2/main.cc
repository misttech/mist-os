// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/sys/cpp/component_context.h>
#include <lib/syslog/cpp/macros.h>

#include "src/lib/fxl/command_line.h"
#include "src/media/audio/audio_core/shared/profile_acquirer.h"
#include "src/media/audio/audio_core/v2/audio_core_component.h"
#include "src/media/audio/services/common/fidl_thread.h"

using ::media_audio::AudioCoreComponent;
using ::media_audio::FidlThread;

namespace {

std::shared_ptr<const FidlThread> CreateMainThread(async_dispatcher_t* dispatcher) {
  auto fidl_thread = FidlThread::CreateFromCurrentThread("fidl_thread", dispatcher);

  // We receive audio payloads over FIDL, which means the FIDL thread has real time requirements
  // just like the mixing threads.
  // TODO(https://fxbug.dev/42181009): the mixer service's graph threads should do this too
  auto result =
      media::audio::AcquireSchedulerRole(zx::thread::self(), "fuchsia.media.audio.core.dispatch");
  if (result.is_error()) {
    FX_PLOGS(ERROR, result.status_value())
        << "Unable to acquire scheduler role for the audio_core FIDL thread";
  }

  return fidl_thread;
}

}  // namespace

int main(int argc, const char** argv) {
  FX_LOGS(INFO) << "AudioCore starting up";

  auto cl = fxl::CommandLineFromArgcArgv(argc, argv);
  const auto enable_cobalt = !cl.HasOption("disable-cobalt");

  // The main thread will serve FIDL requests for all discoverable protocols.
  async::Loop main_loop{&kAsyncLoopConfigAttachToCurrentThread};
  const auto main_thread = CreateMainThread(main_loop.dispatcher());

  // Publish all services.
  component::OutgoingDirectory outgoing(main_loop.dispatcher());
  AudioCoreComponent audio_core(outgoing, main_thread, enable_cobalt);

  // TODO(https://fxbug.dev/42181009): also publish mixer and ADR services

  // Run the FIDL loop.
  if (auto status = outgoing.ServeFromStartupInfo().status_value(); status != ZX_OK) {
    FX_PLOGS(FATAL, status) << "ServeFromStartupInfo failed";
  }
  main_loop.Run();

  // In normal operation this loop will run perpetually, so exiting represents an error condition.
  return -1;
}
