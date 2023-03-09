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

zx::result<> AcquireSchedulerRole(zx::unowned_thread thread, const std::string& role) {
  TRACE_DURATION("audio", "AcquireSchedulerRole", "role", TA_STRING(role.c_str()));

  auto client = ConnectToProfileProvider();
  if (!client.is_ok()) {
    FX_PLOGS(ERROR, client.status_value())
        << "Failed to connect to fuchsia.scheduler.ProfileProvider";
    return zx::error(client.status_value());
  }

  zx::thread dup_thread;
  const zx_status_t dup_status = thread->duplicate(ZX_RIGHT_SAME_RIGHTS, &dup_thread);
  if (dup_status != ZX_OK) {
    FX_PLOGS(ERROR, dup_status) << "Failed to connect to duplicate thread handle";
    return zx::error(dup_status);
  }
  auto result = (*client)->SetProfileByRole({{.thread = std::move(dup_thread), .role = role}});
  if (!result.is_ok()) {
    FX_LOGS(ERROR) << "Failed to call SetProfileByRole, error=" << result.error_value();
    return zx::error(result.error_value().status());
  }
  if (result->status() != ZX_OK) {
    FX_PLOGS(ERROR, result->status()) << "Failed to get set role";
    return zx::error(result->status());
  }

  return zx::ok();
}

}  // namespace media::audio
