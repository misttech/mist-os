// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/audio_core/shared/stream_usage.h"

namespace media::audio {

std::optional<fuchsia::media::AudioRenderUsage2> FidlRenderUsageFromRenderUsage(RenderUsage u) {
  auto underlying = static_cast<std::underlying_type_t<RenderUsage>>(u);
  if (underlying < fuchsia::media::RENDER_USAGE_COUNT) {
    return {static_cast<fuchsia::media::AudioRenderUsage2>(underlying)};
  }
  return {};
}

std::optional<fuchsia::media::AudioCaptureUsage> FidlCaptureUsageFromCaptureUsage(CaptureUsage u) {
  auto underlying = static_cast<std::underlying_type_t<CaptureUsage>>(u);
  if (underlying < fuchsia::media::CAPTURE_USAGE_COUNT) {
    return {static_cast<fuchsia::media::AudioCaptureUsage>(underlying)};
  }
  return {};
}

fuchsia::media::AudioRenderUsage2 ToFidlRenderUsage2(
    const fuchsia::media::AudioRenderUsage& usage) {
  return fuchsia::media::AudioRenderUsage2(fidl::ToUnderlying(usage));
}

fuchsia::media::Usage2 ToFidlUsage2(const fuchsia::media::Usage& usage) {
  if (usage.is_render_usage()) {
    return fuchsia::media::Usage2::WithRenderUsage(ToFidlRenderUsage2(usage.render_usage()));
  }
  return fuchsia::media::Usage2::WithCaptureUsage(fidl::Clone(usage.capture_usage()));
}

std::optional<fuchsia::media::AudioRenderUsage> FromFidlRenderUsage2(
    const fuchsia::media::AudioRenderUsage2& usage2) {
  if (auto index = fidl::ToUnderlying(usage2); index < fuchsia::media::RENDER_USAGE_COUNT) {
    return fuchsia::media::AudioRenderUsage(index);
  }
  return {};
}

std::optional<fuchsia::media::Usage> FromFidlUsage2(const fuchsia::media::Usage2& usage2) {
  if (usage2.is_capture_usage()) {
    return fuchsia::media::Usage::WithCaptureUsage(fidl::Clone(usage2.capture_usage()));
  }
  if (auto index = fidl::ToUnderlying(usage2.render_usage());
      index < fuchsia::media::RENDER_USAGE_COUNT) {
    return fuchsia::media::Usage::WithRenderUsage(fuchsia::media::AudioRenderUsage(index));
  }
  return {};
}

StreamUsage StreamUsageFromFidlUsage(const fuchsia::media::Usage2& usage) {
  if (usage.is_render_usage()) {
    return StreamUsage::WithRenderUsage(usage.render_usage());
  }
  if (usage.is_capture_usage()) {
    return StreamUsage::WithCaptureUsage(usage.capture_usage());
  }
  return StreamUsage();
}

const char* RenderUsageToString(const RenderUsage& usage) {
  switch (usage) {
#define EXPAND_RENDER_USAGE(U) \
  case RenderUsage::U:         \
    return "RenderUsage::" #U;
    EXPAND_EACH_RENDER_USAGE
#undef EXPAND_RENDER_USAGE
  }
}

const char* CaptureUsageToString(const CaptureUsage& usage) {
  switch (usage) {
#define EXPAND_CAPTURE_USAGE(U) \
  case CaptureUsage::U:         \
    return "CaptureUsage::" #U;
    EXPAND_EACH_CAPTURE_USAGE
#undef EXPAND_CAPTURE_USAGE
  }
}

const char* StreamUsage::ToString() const {
  if (is_render_usage()) {
    return RenderUsageToString(render_usage());
  }
  if (is_capture_usage()) {
    return CaptureUsageToString(capture_usage());
  }
  return "(empty usage)";
}

}  // namespace media::audio
