// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/audio_core/shared/profile_acquirer.h"

#include <fidl/fuchsia.scheduler/cpp/fidl.h>
#include <lib/fdio/directory.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/trace/event.h>
#include <lib/zx/channel.h>

#include <cstdlib>

#include <sdk/lib/component/incoming/cpp/protocol.h>

#include "src/media/audio/audio_core/shared/mix_profile_config.h"

namespace media::audio {

namespace {

zx::result<fidl::SyncClient<fuchsia_scheduler::ProfileProvider>> ConnectToProfileProvider() {
  auto client_end_result = component::Connect<fuchsia_scheduler::ProfileProvider>();
  if (!client_end_result.is_ok()) {
    return client_end_result.take_error();
  }
  return zx::ok(fidl::SyncClient(std::move(*client_end_result)));
}

}  // namespace

zx::result<zx::profile> AcquireHighPriorityProfile(const MixProfileConfig& mix_profile_config) {
  TRACE_DURATION("audio", "AcquireHighPriorityProfile");

  auto client = ConnectToProfileProvider();
  if (!client.is_ok()) {
    FX_PLOGS(ERROR, client.status_value())
        << "Failed to connect to fuchsia.scheduler.ProfileProvider";
    return zx::error(client.status_value());
  }

  auto result = (*client)->GetDeadlineProfile({{
      .capacity = static_cast<uint64_t>(mix_profile_config.capacity.get()),
      .deadline = static_cast<uint64_t>(mix_profile_config.deadline.get()),
      .period = static_cast<uint64_t>(mix_profile_config.period.get()),
      .name = "src/media/audio/audio_core",
  }});
  if (!result.is_ok()) {
    FX_LOGS(ERROR) << "Failed to call GetDeadlineProfile, error=" << result.error_value();
    return zx::error(result.error_value().status());
  }
  if (result->status() != ZX_OK) {
    FX_PLOGS(ERROR, result->status()) << "Failed to get deadline profile";
    return zx::error(result->status());
  }

  return zx::ok(std::move(result->profile()));
}

zx::result<zx::profile> AcquireRelativePriorityProfile(uint32_t priority) {
  auto nonce = TRACE_NONCE();
  TRACE_DURATION("audio", "AcquireRelativePriorityProfile");

  auto client = ConnectToProfileProvider();
  if (!client.is_ok()) {
    FX_PLOGS(ERROR, client.status_value())
        << "Failed to connect to fuchsia.scheduler.ProfileProvider";
    return zx::error(client.status_value());
  }

  TRACE_FLOW_BEGIN("audio", "GetProfile", nonce);
  auto result = (*client)->GetProfile({{
      .priority = priority,
      .name = "src/media/audio/audio_core",
  }});
  TRACE_FLOW_END("audio", "GetProfile", nonce);

  if (!result.is_ok()) {
    FX_LOGS(ERROR) << "Failed to call GetProfile, error=" << result.error_value();
    return zx::error(result.error_value().status());
  }
  if (result->status() != ZX_OK) {
    FX_PLOGS(ERROR, result->status()) << "Failed to get profile";
    return zx::error(result->status());
  }

  return zx::ok(std::move(result->profile()));
}

zx::result<zx::profile> AcquireAudioCoreImplProfile() {
  return AcquireRelativePriorityProfile(/* HIGH_PRIORITY in zircon */ 24);
}

}  // namespace media::audio
