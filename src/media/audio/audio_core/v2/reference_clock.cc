// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/audio_core/v2/reference_clock.h"

#include <fidl/fuchsia.hardware.audio/cpp/fidl.h>
#include <lib/syslog/cpp/macros.h>

#include "src/media/audio/lib/clock/utils.h"

namespace media_audio {

using ::media::audio::clock::DuplicateClock;

// static
ReferenceClock ReferenceClock::FromMonotonic() {
  zx::clock mono;
  auto status = zx::clock::create(
      ZX_CLOCK_OPT_AUTO_START | ZX_CLOCK_OPT_MONOTONIC | ZX_CLOCK_OPT_CONTINUOUS, nullptr, &mono);
  if (status != ZX_OK) {
    FX_PLOGS(FATAL, status) << "zx::clock::create failed for system monotonic clock";
  }

  return {
      .name = "Monotonic",
      .handle = std::move(mono),
      .domain = fuchsia_hardware_audio::kClockDomainMonotonic,
  };
}

// static
ReferenceClock ReferenceClock::FromFidlRingBuffer(fuchsia_audio::wire::RingBuffer ring_buffer) {
  return {
      .handle = DuplicateClock(ring_buffer.reference_clock()),
      .domain = ring_buffer.has_reference_clock_domain()
                    ? ring_buffer.reference_clock_domain()
                    : fuchsia_hardware_audio::kClockDomainExternal,
  };
}

ReferenceClock ReferenceClock::Dup() const {
  return {
      .name = name,
      .handle = DuplicateClock(handle),
      .domain = domain,
  };
}

zx::clock ReferenceClock::DupHandle() const { return DuplicateClock(handle); }

fuchsia_audio_mixer::wire::ReferenceClock ReferenceClock::ToFidl(fidl::AnyArena& arena) const {
  auto builder = fuchsia_audio_mixer::wire::ReferenceClock::Builder(arena);
  if (!name.empty()) {
    builder.name(fidl::StringView(arena, name));
  }
  builder.handle(DuplicateClock(handle));
  builder.domain(domain);
  return builder.Build();
}

}  // namespace media_audio
