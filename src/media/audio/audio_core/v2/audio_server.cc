// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/audio_core/v2/audio_server.h"

#include <lib/syslog/cpp/macros.h>
#include <lib/trace/event.h>

namespace media_audio {

using ::media::audio::CaptureUsage;
using ::media::audio::RenderUsage;

// static
std::shared_ptr<AudioServer> AudioServer::Create(std::shared_ptr<const FidlThread> fidl_thread,
                                                 fidl::ServerEnd<fuchsia_media::Audio> server_end,
                                                 std::shared_ptr<RendererCapturerCreator> creator) {
  return BaseFidlServer::Create(fidl_thread, std::move(server_end), std::move(creator));
}

void AudioServer::CreateAudioRenderer(CreateAudioRendererRequestView request,
                                      CreateAudioRendererCompleter::Sync& completer) {
  TRACE_DURATION("audio", "AudioServer::CreateAudioRenderer");

  if (!request->audio_renderer_request) {
    FX_LOGS(WARNING) << "CreateAudioRenderer: invalid handle";
    Shutdown(ZX_ERR_INVALID_ARGS);
    return;
  }

  creator_->CreateRenderer(std::move(request->audio_renderer_request), RenderUsage::MEDIA,
                           /*format=*/std::nullopt, /*notify_clock=*/nullptr);
}

void AudioServer::CreateAudioCapturer(CreateAudioCapturerRequestView request,
                                      CreateAudioCapturerCompleter::Sync& completer) {
  TRACE_DURATION("audio", "AudioServer::CreateAudioCapturer");

  if (!request->audio_capturer_request) {
    FX_LOGS(WARNING) << "CreateAudioCapturer: invalid handle";
    Shutdown(ZX_ERR_INVALID_ARGS);
    return;
  }

  creator_->CreateCapturer(std::move(request->audio_capturer_request),
                           request->loopback ? CaptureUsage::LOOPBACK : CaptureUsage::FOREGROUND,
                           /*format=*/std::nullopt, /*notify_clock=*/nullptr);
}

}  // namespace media_audio
