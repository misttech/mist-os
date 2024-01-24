// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.kernel/cpp/wire.h>
#include <fidl/fuchsia.scheduler/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/profile.h>
#include <lib/zx/thread.h>
#include <zircon/syscalls.h>
#include <zircon/syscalls/object.h>

#include "zircon/syscalls/profile.h"
#include "zircon/system/ulib/profile/config.h"

namespace {

constexpr char kConfigPath[] = "/config/profiles";

using zircon_profile::ConfiguredProfiles;
using zircon_profile::Role;

zx::result<zx::resource> GetSystemProfileResource() {
  zx::result client = component::Connect<fuchsia_kernel::ProfileResource>();
  if (client.is_error()) {
    return client.take_error();
  }
  fidl::WireResult result = fidl::WireCall(*client)->Get();
  if (result.status() != ZX_OK) {
    return zx::error(result.status());
  }
  return zx::ok(std::move(result.value().resource));
}

class RoleManager : public fidl::WireServer<fuchsia_scheduler::RoleManager> {
 public:
  static zx::result<std::unique_ptr<RoleManager>> Create();
  void SetRole(SetRoleRequestView request, SetRoleCompleter::Sync& completer) override;
  void handle_unknown_method(fidl::UnknownMethodMetadata<fuchsia_scheduler::RoleManager> metadata,
                             fidl::UnknownMethodCompleter::Sync& completer) override;

 private:
  RoleManager(zx::resource profile_resource, ConfiguredProfiles profiles)
      : profile_resource_(std::move(profile_resource)), profiles_(std::move(profiles)) {}
  zx::resource profile_resource_;
  ConfiguredProfiles profiles_;
};

zx::result<std::unique_ptr<RoleManager>> RoleManager::Create() {
  auto profile_resource_result = GetSystemProfileResource();
  if (profile_resource_result.is_error()) {
    FX_LOGS(ERROR) << "failed to get profile resource: " << profile_resource_result.status_string();
    return profile_resource_result.take_error();
  }
  zx::resource profile_resource = std::move(profile_resource_result.value());

  auto config_result = zircon_profile::LoadConfigs(kConfigPath);
  if (config_result.is_error()) {
    FX_SLOG(ERROR, "Failed to load configs", FX_KV("error", config_result.error_value()),
            FX_KV("tag", "RoleManager"));
    return zx::error(ZX_ERR_INTERNAL);
  }

  auto create = [&profile_resource](zircon_profile::ProfileMap& profiles) {
    // Create profiles for each configured role. If creating the profile fails, remove the role
    // entry.
    for (auto iter = profiles.begin(); iter != profiles.end();) {
      const zx_status_t status =
          zx::profile::create(profile_resource, 0, &iter->second.info, &iter->second.profile);
      if (status != ZX_OK) {
        FX_SLOG(ERROR, "Failed to create profile for role. Requests for this role will fail.",
                FX_KV("role", iter->first.name()), FX_KV("status", zx_status_get_string(status)));
        iter = profiles.erase(iter);
      } else {
        ++iter;
      }
    }
  };
  create(config_result->thread);
  create(config_result->memory);

  // Apply the dispatch role if defined.
  const std::string dispatch_role_name = "fuchsia.system.profile-provider.dispatch";
  const fit::result dispatch_role = Role::Create(dispatch_role_name);
  if (dispatch_role.is_error()) {
    FX_SLOG(ERROR, "Failed to parse dispatch role.",
            FX_KV("error", zx_status_get_string(dispatch_role.error_value())),
            FX_KV("tag", "ProfileProvider"));
  }
  const auto search = config_result->thread.find(*dispatch_role);
  if (search != config_result->thread.end()) {
    const zx_status_t status = zx::thread::self()->set_profile(search->second.profile, 0);
    if (status != ZX_OK) {
      FX_SLOG(ERROR, "Failed to set role", FX_KV("error", zx_status_get_string(status)),
              FX_KV("tag", "RoleManager"));
    }
  }

  return zx::ok(std::unique_ptr<RoleManager>(
      new RoleManager{std::move(profile_resource), std::move(config_result.value())}));
}

void RoleManager::handle_unknown_method(
    fidl::UnknownMethodMetadata<fuchsia_scheduler::RoleManager> metadata,
    fidl::UnknownMethodCompleter::Sync& completer) {
  FX_SLOG(ERROR, "Got request to handle unknown method", FX_KV("tag", "RoleManager"));
}

void RoleManager::SetRole(SetRoleRequestView request, SetRoleCompleter::Sync& completer) {
  // Log the requested role and PID:TID of the thread or vmar being assigned.
  const std::string_view role_name{request->role().role.get()};
  zx_status_t status = ZX_OK;
  zx_handle_t target_handle = ZX_HANDLE_INVALID;
  if (request->target().is_thread()) {
    target_handle = request->target().thread().get();
    zx_info_handle_basic_t handle_info{};
    status = request->target().thread().get_info(ZX_INFO_HANDLE_BASIC, &handle_info,
                                                 sizeof(handle_info), nullptr, nullptr);
    if (status != ZX_OK) {
      completer.ReplyError(status);
      return;
    }
    FX_SLOG(DEBUG, "Role requested for thread:", FX_KV("role", role_name),
            FX_KV("pid", handle_info.related_koid), FX_KV("tid", handle_info.koid),
            FX_KV("tag", "RoleManager"));
  } else if (request->target().is_vmar()) {
    target_handle = request->target().vmar().get();
    zx_info_handle_basic_t handle_info{};
    zx_status_t status = request->target().vmar().get_info(ZX_INFO_HANDLE_BASIC, &handle_info,
                                                           sizeof(handle_info), nullptr, nullptr);
    if (status != ZX_OK) {
      completer.ReplyError(status);
      return;
    }
    FX_SLOG(DEBUG, "Role requested for vmar:", FX_KV("role", role_name),
            FX_KV("pid", handle_info.related_koid), FX_KV("koid", handle_info.koid),
            FX_KV("tag", "RoleManager"));
  } else {
    completer.ReplyError(ZX_ERR_INVALID_ARGS);
    return;
  }

  std::optional<std::vector<fuchsia_scheduler::Parameter>> input_params =
      fidl::ToNatural(request->input_parameters());
  if (!input_params.has_value()) {
    FX_SLOG(WARNING, "Unable to take ownership of input parameters.", FX_KV("role", role_name),
            FX_KV("tag", "RoleManager"));
    completer.ReplyError(ZX_ERR_INVALID_ARGS);
    return;
  }
  const fit::result role = Role::Create(role_name, input_params.value());
  if (role.is_error()) {
    completer.ReplyError(role.error_value());
    return;
  }

  const auto& profile_map = request->target().is_thread() ? profiles_.thread : profiles_.memory;

  // Select the profile parameters based on the role selector.
  fidl::Arena arena;
  auto builder = fuchsia_scheduler::wire::RoleManagerSetRoleResponse::Builder(arena);

  // Handle the test role case specially.
  if (role->IsTestRole()) {
    if (role->HasSelector("not-found")) {
      completer.ReplyError(ZX_ERR_NOT_FOUND);
    } else if (role->HasSelector("ok")) {
      completer.ReplySuccess(builder.Build());
    } else {
      completer.ReplyError(ZX_ERR_INVALID_ARGS);
    }
    return;
  }

  // Look for the requested role in the profile map and set the profile if found.
  if (auto search = profile_map.find(*role); search != profile_map.cend()) {
    status = zx_object_set_profile(target_handle, search->second.profile.get(), 0);
    if (status != ZX_OK) {
      completer.ReplyError(status);
      return;
    }
    builder.output_parameters(fidl::ToWire(arena, search->second.output_parameters));
    completer.ReplySuccess(builder.Build());
    return;
  }

  FX_SLOG(DEBUG, "Requested role not found", FX_KV("role", role->name()),
          FX_KV("tag", "RoleManager"));
  completer.ReplyError(ZX_ERR_NOT_FOUND);
}

}  // namespace

int main(int argc, const char** argv) {
  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);
  async_dispatcher_t* dispatcher = loop.dispatcher();

  zx::result create_result = RoleManager::Create();
  if (create_result.is_error()) {
    FX_LOGS(ERROR) << "failed to create role manager service: " << create_result.status_string();
    return -1;
  }
  std::unique_ptr<RoleManager> role_manager_service = std::move(create_result.value());

  component::OutgoingDirectory outgoing = component::OutgoingDirectory(dispatcher);
  zx::result result =
      outgoing.AddProtocol<fuchsia_scheduler::RoleManager>(std::move(role_manager_service));
  if (result.is_error()) {
    FX_LOGS(ERROR) << "failed to add RoleManager protocol: " << result.status_string();
    return -1;
  }

  result = outgoing.ServeFromStartupInfo();
  if (result.is_error()) {
    FX_LOGS(ERROR) << "failed to serve outgoing directory: " << result.status_string();
    return -1;
  }
  FX_LOGS(INFO) << "starting role manager\n";
  zx_status_t status = loop.Run();
  if (status != ZX_OK) {
    FX_LOGS(ERROR) << "failed to run async loop: " << zx_status_get_string(status);
  }
  return 0;
}
