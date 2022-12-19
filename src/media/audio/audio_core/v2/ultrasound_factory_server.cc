// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/audio_core/v2/ultrasound_factory_server.h"

#include <lib/syslog/cpp/macros.h>
#include <lib/trace/event.h>

#include "src/media/audio/audio_core/v2/audio_capturer_server.h"
#include "src/media/audio/audio_core/v2/audio_renderer_server.h"

namespace media_audio {

namespace {

using ::media::audio::CaptureUsage;
using ::media::audio::RenderUsage;

// TODO(fxbug.dev/98652): This code is duplicated in multiple places. Put a single copy in
// src/media/audio/lib/clock
zx::clock DupZxClockHandle(const zx::clock& in) {
  zx::clock out;
  if (auto status = in.duplicate(ZX_RIGHT_SAME_RIGHTS, &out); status != ZX_OK) {
    FX_PLOGS(FATAL, status) << "zx::clock::duplicate failed";
  }
  return out;
}

}  // namespace

// static
std::shared_ptr<UltrasoundFactoryServer> UltrasoundFactoryServer::Create(
    std::shared_ptr<const FidlThread> fidl_thread,
    fidl::ServerEnd<fuchsia_ultrasound::Factory> server_end, Args args) {
  return BaseFidlServer::Create(std::move(fidl_thread), std::move(server_end), std::move(args));
}

void UltrasoundFactoryServer::CreateRenderer(CreateRendererRequestView request,
                                             CreateRendererCompleter::Sync& completer) {
  TRACE_DURATION("audio", "UltrasoundFactoryServer::CreateRenderer");

  if (!request->renderer) {
    FX_LOGS(WARNING) << "CreateRenderer: invalid handle";
    Shutdown(ZX_ERR_INVALID_ARGS);
    return;
  }

  creator_->CreateRenderer(
      std::move(request->renderer), RenderUsage::ULTRASOUND, renderer_format_,
      [format = renderer_format_, completer = completer.ToAsync()](const auto& clock) mutable {
        completer.Reply(DupZxClockHandle(clock), format.ToLegacyMediaWireFidl());
      });
}

void UltrasoundFactoryServer::CreateCapturer(CreateCapturerRequestView request,
                                             CreateCapturerCompleter::Sync& completer) {
  TRACE_DURATION("audio", "UltrasoundFactoryServer::CreateCapturer");

  if (!request->request) {
    FX_LOGS(WARNING) << "CreateCapturer: invalid handle";
    Shutdown(ZX_ERR_INVALID_ARGS);
    return;
  }

  creator_->CreateCapturer(
      std::move(request->request), CaptureUsage::ULTRASOUND, capturer_format_,
      [format = capturer_format_, completer = completer.ToAsync()](const auto& clock) mutable {
        completer.Reply(DupZxClockHandle(clock), format.ToLegacyMediaWireFidl());
      });
}

}  // namespace media_audio
