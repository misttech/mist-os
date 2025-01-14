// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/audio_core/shared/stream_usage.h"

#include <fidl/fuchsia.media/cpp/fidl.h>
#include <fidl/fuchsia.media/cpp/hlcpp_conversion.h>
#include <fidl/fuchsia.media/cpp/type_conversions.h>
#include <fuchsia/media/cpp/fidl.h>
#include <lib/fidl/cpp/hlcpp_conversion.h>
#include <lib/fidl/cpp/wire_natural_conversions.h>

namespace media::audio {

using fuchsia::media::AudioCaptureUsage;
using fuchsia::media::AudioCaptureUsage2;
using fuchsia::media::AudioRenderUsage;
using fuchsia::media::AudioRenderUsage2;

//
// Conversions
// Convenience functions for when the conversion is guaranteed to succeed (e.g. in test setup).
//

// RenderUsage
RenderUsage ToRenderUsage(AudioRenderUsage u) { return RenderUsage(ToIndex(u)); }
RenderUsage ToRenderUsage(fuchsia_media::AudioRenderUsage u) { return RenderUsage(ToIndex(u)); }
RenderUsage ToRenderUsage(AudioRenderUsage2 u) { return RenderUsage(ToIndex(u)); }
RenderUsage ToRenderUsage(fuchsia_media::AudioRenderUsage2 u) { return RenderUsage(ToIndex(u)); }

// CaptureUsage
CaptureUsage ToCaptureUsage(AudioCaptureUsage usage) { return CaptureUsage(ToIndex(usage)); }
CaptureUsage ToCaptureUsage(fuchsia_media::AudioCaptureUsage usage) {
  return CaptureUsage(ToIndex(usage));
}
CaptureUsage ToCaptureUsage(AudioCaptureUsage2 usage) { return CaptureUsage(ToIndex(usage)); }
CaptureUsage ToCaptureUsage(fuchsia_media::AudioCaptureUsage2 usage) {
  return CaptureUsage(ToIndex(usage));
}

const char* ToString(const RenderUsage& usage) {
  switch (usage) {
#define EXPAND_RENDER_USAGE(U) \
  case RenderUsage::U:         \
    return "RenderUsage::" #U;
    EXPAND_EACH_RENDER_USAGE
#undef EXPAND_RENDER_USAGE
  }
}

const char* ToString(const CaptureUsage& usage) {
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
    return media::audio::ToString(render_usage());
  }
  if (is_capture_usage()) {
    return media::audio::ToString(capture_usage());
  }
  return "(empty usage)";
}

// StreamUsage
StreamUsage ToStreamUsage(const fuchsia::media::Usage2& usage) {
  if (usage.is_render_usage()) {
    return StreamUsage::WithRenderUsage(usage.render_usage());
  }
  if (usage.is_capture_usage()) {
    return StreamUsage::WithCaptureUsage(usage.capture_usage());
  }
  return StreamUsage();
}

// AudioRenderUsage
std::optional<AudioRenderUsage> ToFidlRenderUsageTry(const AudioRenderUsage2& usage2) {
  if (auto index = ToIndex(usage2); index < fuchsia::media::RENDER_USAGE_COUNT) {
    return AudioRenderUsage(index);
  }
  return {};
}

// AudioRenderUsage2
AudioRenderUsage2 ToFidlRenderUsage2(const AudioRenderUsage& usage) {
  return AudioRenderUsage2(fidl::ToUnderlying(usage));
}
AudioRenderUsage2 ToFidlRenderUsage2(const fuchsia_media::AudioRenderUsage& usage) {
  return AudioRenderUsage2(fidl::ToUnderlying(usage));
}
AudioRenderUsage2 ToFidlRenderUsage2(const fuchsia_media::AudioRenderUsage2& usage) {
  return AudioRenderUsage2(fidl::ToUnderlying(usage));
}
AudioRenderUsage2 ToFidlRenderUsage2(RenderUsage u) {
  auto underlying = static_cast<std::underlying_type_t<RenderUsage>>(u);
  return {AudioRenderUsage2(underlying)};
}

// AudioCaptureUsage
std::optional<AudioCaptureUsage> ToFidlCaptureUsageTry(const AudioCaptureUsage2& usage2) {
  if (auto index = ToIndex(usage2); index < fuchsia::media::CAPTURE_USAGE_COUNT) {
    return AudioCaptureUsage(index);
  }
  return {};
}
AudioCaptureUsage ToFidlCaptureUsage(const fuchsia_media::AudioCaptureUsage& usage) {
  return AudioCaptureUsage(fidl::ToUnderlying(usage));
}
AudioCaptureUsage ToFidlCaptureUsage(CaptureUsage usage) {
  auto underlying = static_cast<std::underlying_type_t<CaptureUsage>>(usage);
  return AudioCaptureUsage(underlying);
}

// AudioCaptureUsage2
AudioCaptureUsage2 ToFidlCaptureUsage2(const AudioCaptureUsage& usage) {
  return AudioCaptureUsage2(fidl::ToUnderlying(usage));
}
AudioCaptureUsage2 ToFidlCaptureUsage2(const fuchsia_media::AudioCaptureUsage& usage) {
  return AudioCaptureUsage2(fidl::ToUnderlying(usage));
}
AudioCaptureUsage2 ToFidlCaptureUsage2(const fuchsia_media::AudioCaptureUsage2& usage) {
  return AudioCaptureUsage2(fidl::ToUnderlying(usage));
}
fuchsia::media::AudioCaptureUsage2 ToFidlCaptureUsage2(CaptureUsage usage) {
  auto underlying = static_cast<std::underlying_type_t<CaptureUsage>>(usage);
  return {AudioCaptureUsage2(underlying)};
}

// Usage
std::optional<fuchsia::media::Usage> ToFidlUsageTry(const fuchsia::media::Usage2& usage2) {
  if (usage2.is_capture_usage()) {
    auto index = ToIndex(usage2.capture_usage());
    if (index < fuchsia::media::CAPTURE_USAGE_COUNT) {
      return fuchsia::media::Usage::WithCaptureUsage(AudioCaptureUsage(index));
    }
  } else if (auto index = ToIndex(usage2.render_usage());
             index < fuchsia::media::RENDER_USAGE_COUNT) {
    return fuchsia::media::Usage::WithRenderUsage(AudioRenderUsage(index));
  }
  return {};
}

// Usage2
fuchsia::media::Usage2 ToFidlUsage2(const fuchsia::media::Usage& usage) {
  if (usage.is_render_usage()) {
    return fuchsia::media::Usage2::WithRenderUsage(ToFidlRenderUsage2(usage.render_usage()));
  }
  return fuchsia::media::Usage2::WithCaptureUsage(ToFidlCaptureUsage2(usage.capture_usage()));
}
fuchsia::media::Usage2 ToFidlUsage2(const fuchsia_media::Usage& usage) {
  if (usage.Which() == fuchsia_media::Usage::Tag::kRenderUsage) {
    return fuchsia::media::Usage2::WithRenderUsage(
        ToFidlRenderUsage2(usage.render_usage().value()));
  }
  return fuchsia::media::Usage2::WithCaptureUsage(
      ToFidlCaptureUsage2(usage.capture_usage().value()));
}
fuchsia::media::Usage2 ToFidlUsage2(RenderUsage usage) {
  auto underlying = static_cast<std::underlying_type_t<RenderUsage>>(usage);
  FX_CHECK(underlying < fuchsia::media::RENDER_USAGE2_COUNT);
  return fuchsia::media::Usage2::WithRenderUsage(AudioRenderUsage2(underlying));
}
fuchsia::media::Usage2 ToFidlUsage2(CaptureUsage usage) {
  auto underlying = static_cast<std::underlying_type_t<CaptureUsage>>(usage);
  FX_CHECK(underlying < fuchsia::media::CAPTURE_USAGE_COUNT);
  return fuchsia::media::Usage2::WithCaptureUsage(AudioCaptureUsage2(underlying));
}

}  // namespace media::audio
