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

zx::result<> ApplyProfile(zx::unowned_handle handle, const std::string& role) {
  auto client = ConnectToProfileProvider();
  if (!client.is_ok()) {
    FX_PLOGS(ERROR, client.status_value())
        << "Failed to connect to fuchsia.scheduler.ProfileProvider";
    return zx::error(client.status_value());
  }

  zx::handle dup_handle;
  const zx_status_t dup_status = handle->duplicate(ZX_RIGHT_SAME_RIGHTS, &dup_handle);
  if (dup_status != ZX_OK) {
    FX_PLOGS(ERROR, dup_status) << "Failed to duplicate handle";
    return zx::error(dup_status);
  }

  auto result = (*client)->SetProfileByRole({{.handle = std::move(dup_handle), .role = role}});
  if (!result.is_ok()) {
    FX_LOGS(ERROR) << "Failed to call SetProfileByRole, error=" << result.error_value();
    return zx::error(result.error_value().status());
  }
  if (result->status() != ZX_OK) {
    FX_PLOGS(ERROR, result->status()) << "Failed to set profile based on this role: " << role;
    return zx::error(result->status());
  }
  return zx::ok();
}

}  // namespace

zx::result<> AcquireSchedulerRole(zx::unowned_thread thread, const std::string& role) {
  TRACE_DURATION("audio", "AcquireSchedulerRole", "role", TA_STRING(role.c_str()));
  return ApplyProfile(zx::unowned_handle(thread->get()), role);
}

zx::result<> AcquireMemoryRole(zx::unowned_vmar vmar, const std::string& role) {
  TRACE_DURATION("audio", "AcquireMemoryRole", "role", TA_STRING(role.c_str()));
  return ApplyProfile(zx::unowned_handle(vmar->get()), role);
}

}  // namespace media::audio
