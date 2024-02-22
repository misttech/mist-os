// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.scheduler/cpp/fidl.h>
#include <lib/scheduler/role.h>

#include <shared_mutex>

#include <sdk/lib/component/incoming/cpp/protocol.h>

namespace {

class RoleClient {
 public:
  RoleClient() = default;
  zx::result<std::shared_ptr<fidl::WireSyncClient<fuchsia_scheduler::ProfileProvider>>> Connect();
  void Disconnect();

 private:
  // Keeps track of a synchronous connection to the ProfileProvider service.
  // This shared pointer is set to nullptr if the connection has been terminated.
  std::shared_ptr<fidl::WireSyncClient<fuchsia_scheduler::ProfileProvider>> profile_provider_;
  std::shared_mutex mutex_;
};

zx::result<std::shared_ptr<fidl::WireSyncClient<fuchsia_scheduler::ProfileProvider>>>
RoleClient::Connect() {
  // Check if we already have a connection to the profile provider, and return it if we do.
  // This requires acquiring the read lock.
  {
    std::shared_lock lock(mutex_);
    if (profile_provider_ != nullptr) {
      return zx::ok(profile_provider_);
    }
  }

  // At this point, we don't have an existing connection to the profile provider, so we need to
  // create one. This requires the following set of operations to happen in order.
  // 1. Acquire the write lock. This will prevent concurrent connection attempts.
  // 2. Make sure that another thread didn't already connect the client by the time we got the write
  //    lock.
  // 3. Establish the connection.
  std::unique_lock lock(mutex_);
  if (profile_provider_ != nullptr) {
    return zx::ok(profile_provider_);
  }
  auto client_end_result = component::Connect<fuchsia_scheduler::ProfileProvider>();
  if (!client_end_result.is_ok()) {
    return client_end_result.take_error();
  }
  profile_provider_ = std::make_shared<fidl::WireSyncClient<fuchsia_scheduler::ProfileProvider>>(
      fidl::WireSyncClient(std::move(*client_end_result)));
  return zx::ok(profile_provider_);
}

void RoleClient::Disconnect() {
  std::unique_lock lock(mutex_);
  profile_provider_ = nullptr;
}

// Stores a persistent connection to the ProfileProvider that reconnects on disconnect.
static RoleClient role_client{};

zx_status_t SetRole(const zx_handle_t borrowed_handle, std::string_view role) {
// TODO(https://fxbug.dev/323262398): Remove this check once the necessary API is in the SDK.
#if __Fuchsia_API_level__ < FUCHSIA_HEAD
  return ZX_ERR_NOT_SUPPORTED;
#endif  // #if __Fuchsia_API_level__ < FUCHSIA_HEAD
  zx::result client = role_client.Connect();
  if (!client.is_ok()) {
    return client.error_value();
  }

  zx::handle handle;
  const zx_status_t dup_status =
      zx_handle_duplicate(borrowed_handle, ZX_RIGHT_SAME_RIGHTS, handle.reset_and_get_address());
  if (dup_status != ZX_OK) {
    return dup_status;
  }

  fidl::WireResult result =
      (*(*client))->SetProfileByRole(std::move(handle), fidl::StringView::FromExternal(role));
  if (!result.ok()) {
    // If the service closed the connection, disconnect the client. This will ensure that future
    // callers of SetRole reconnect to the ProfileProvider service.
    if (result.status() == ZX_ERR_PEER_CLOSED) {
      role_client.Disconnect();
    }
    return result.status();
  }
  return result->status;
}

}  // anonymous namespace

namespace fuchsia_scheduler {

zx_status_t SetRoleForVmar(zx::unowned_vmar vmar, std::string_view role) {
  return SetRole(vmar->get(), role);
}

zx_status_t SetRoleForThread(zx::unowned_thread thread, std::string_view role) {
  return SetRole(thread->get(), role);
}

zx_status_t SetRoleForThisThread(std::string_view role) {
  return SetRole(zx::thread::self()->get(), role);
}

}  // namespace fuchsia_scheduler
