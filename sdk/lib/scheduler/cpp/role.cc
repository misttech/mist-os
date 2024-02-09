// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.scheduler/cpp/fidl.h>
#include <lib/scheduler/role.h>
#include <lib/syslog/cpp/log_settings.h>
#include <lib/syslog/cpp/macros.h>

#include <sdk/lib/component/incoming/cpp/protocol.h>

namespace {

zx::result<fidl::WireSyncClient<fuchsia_scheduler::ProfileProvider>> ConnectToProfileProvider() {
  auto client_end_result = component::Connect<fuchsia_scheduler::ProfileProvider>();
  if (!client_end_result.is_ok()) {
    return client_end_result.take_error();
  }
  return zx::ok(fidl::WireSyncClient(std::move(*client_end_result)));
}

zx_status_t SetRole(const zx_handle_t borrowed_handle, std::string_view role) {
  static zx::result client = ConnectToProfileProvider();
  if (!client.is_ok()) {
    FX_PLOGS(WARNING, client.status_value())
        << "Failed to connect to fuchsia.scheduler.ProfileProvider";
    return client.error_value();
  }

  zx::handle handle;
  const zx_status_t dup_status =
      zx_handle_duplicate(borrowed_handle, ZX_RIGHT_SAME_RIGHTS, handle.reset_and_get_address());
  if (dup_status != ZX_OK) {
    FX_PLOGS(ERROR, dup_status) << "Failed to duplicate thread handle";
    return dup_status;
  }

  fidl::WireResult result =
      (*client)->SetProfileByRole(std::move(handle), fidl::StringView::FromExternal(role));
  if (!result.ok()) {
    FX_LOGS(WARNING) << "Failed to call SetProfileByRole, error=" << result.error()
                     << ". This may be expected if the component does not have access to "
                        "fuchsia.scheduler.ProfileProvider";
    return result.status();
  }
  if (result->status != ZX_OK) {
    FX_LOGS(WARNING) << "Failed to SetProfileByRole, error=" << result->status;
  }
  return result->status;
}

}  // anonymous namespace

namespace fuchsia_scheduler {

zx_status_t SetRoleForHandle(zx::unowned_handle handle, std::string_view role) {
  return SetRole(handle->get(), role);
}

zx_status_t SetRoleForThread(zx::unowned_thread thread, std::string_view role) {
  return SetRole(thread->get(), role);
}

zx_status_t SetRoleForThisThread(std::string_view role) {
  return SetRole(zx::thread::self()->get(), role);
}

}  // namespace fuchsia_scheduler
