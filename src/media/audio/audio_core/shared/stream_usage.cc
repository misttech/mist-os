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

//
// Conversions
// Convenience functions for when the conversion is guaranteed to succeed (e.g. in test setup).
//

// AudioRenderUsage
std::optional<fuchsia::media::AudioRenderUsage> ToFidlRenderUsageTry(
    const fuchsia::media::AudioRenderUsage2& usage2) {
  if (auto index = ToIndex(usage2); index < fuchsia::media::RENDER_USAGE_COUNT) {
    return fuchsia::media::AudioRenderUsage(index);
  }
  return {};
}

// AudioRenderUsage2
fuchsia::media::AudioRenderUsage2 ToFidlRenderUsage2(
    const fuchsia::media::AudioRenderUsage& usage) {
  return fuchsia::media::AudioRenderUsage2(fidl::ToUnderlying(usage));
}
fuchsia::media::AudioRenderUsage2 ToFidlRenderUsage2(const fuchsia_media::AudioRenderUsage& usage) {
  return fuchsia::media::AudioRenderUsage2(fidl::ToUnderlying(usage));
}
fuchsia::media::AudioRenderUsage2 ToFidlRenderUsage2(
    const fuchsia_media::AudioRenderUsage2& usage) {
  return fuchsia::media::AudioRenderUsage2(fidl::ToUnderlying(usage));
}
fuchsia::media::AudioRenderUsage2 ToFidlRenderUsage2(RenderUsage u) {
  auto underlying = static_cast<std::underlying_type_t<RenderUsage>>(u);
  return {fuchsia::media::AudioRenderUsage2(underlying)};
}

// AudioCaptureUsage
fuchsia::media::AudioCaptureUsage ToFidlCaptureUsage(
    const fuchsia_media::AudioCaptureUsage& usage) {
  return fuchsia::media::AudioCaptureUsage(fidl::ToUnderlying(usage));
}

fuchsia::media::AudioCaptureUsage ToFidlCaptureUsage(CaptureUsage usage) {
  auto underlying = static_cast<std::underlying_type_t<CaptureUsage>>(usage);
  return fuchsia::media::AudioCaptureUsage(underlying);
}

// AudioCaptureUsage2

// Usage
std::optional<fuchsia::media::Usage> ToFidlUsageTry(const fuchsia::media::Usage2& usage2) {
  if (usage2.is_capture_usage()) {
    auto index = ToIndex(usage2.capture_usage());
    if (index < fuchsia::media::CAPTURE_USAGE_COUNT) {
      return fuchsia::media::Usage::WithCaptureUsage(fuchsia::media::AudioCaptureUsage(index));
    }
  } else if (auto index = ToIndex(usage2.render_usage());
             index < fuchsia::media::RENDER_USAGE_COUNT) {
    return fuchsia::media::Usage::WithRenderUsage(fuchsia::media::AudioRenderUsage(index));
  }
  return {};
}

// Usage2
fuchsia::media::Usage2 ToFidlUsage2(const fuchsia::media::Usage& usage) {
  if (usage.is_render_usage()) {
    return fuchsia::media::Usage2::WithRenderUsage(ToFidlRenderUsage2(usage.render_usage()));
  }
  return fuchsia::media::Usage2::WithCaptureUsage(fidl::Clone(usage.capture_usage()));
}
fuchsia::media::Usage2 ToFidlUsage2(const fuchsia_media::Usage& usage) {
  if (usage.Which() == fuchsia_media::Usage::Tag::kRenderUsage) {
    return fuchsia::media::Usage2::WithRenderUsage(
        ToFidlRenderUsage2(usage.render_usage().value()));
  }
  return fuchsia::media::Usage2::WithCaptureUsage(
      ToFidlCaptureUsage(usage.capture_usage().value()));
}
fuchsia::media::Usage2 ToFidlUsage2(RenderUsage usage) {
  auto underlying = static_cast<std::underlying_type_t<RenderUsage>>(usage);
  FX_CHECK(underlying < fuchsia::media::RENDER_USAGE2_COUNT);
  return fuchsia::media::Usage2::WithRenderUsage(fuchsia::media::AudioRenderUsage2(underlying));
}
fuchsia::media::Usage2 ToFidlUsage2(CaptureUsage usage) {
  auto underlying = static_cast<std::underlying_type_t<CaptureUsage>>(usage);
  FX_CHECK(underlying < fuchsia::media::CAPTURE_USAGE_COUNT);
  return fuchsia::media::Usage2::WithCaptureUsage(fuchsia::media::AudioCaptureUsage(underlying));
}

// RenderUsage
RenderUsage ToRenderUsage(fuchsia_media::AudioRenderUsage u) { return RenderUsage(ToIndex(u)); }
RenderUsage ToRenderUsage(fuchsia::media::AudioRenderUsage2 u) { return RenderUsage(ToIndex(u)); }
RenderUsage ToRenderUsage(fuchsia_media::AudioRenderUsage2 u) { return RenderUsage(ToIndex(u)); }

// CaptureUsage
CaptureUsage ToCaptureUsage(fuchsia::media::AudioCaptureUsage usage) {
  return CaptureUsage(ToIndex(usage));
}
CaptureUsage ToCaptureUsage(fuchsia_media::AudioCaptureUsage usage) {
  return CaptureUsage(ToIndex(usage));
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

}  // namespace media::audio
