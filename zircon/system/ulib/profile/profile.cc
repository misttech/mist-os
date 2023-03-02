// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "zircon/syscalls/profile.h"

#include <fidl/fuchsia.scheduler/cpp/wire.h>
#include <inttypes.h>
#include <lib/fit/result.h>
#include <lib/profile/profile.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/profile.h>
#include <lib/zx/thread.h>
#include <string.h>
#include <zircon/assert.h>
#include <zircon/errors.h>
#include <zircon/status.h>
#include <zircon/syscalls.h>
#include <zircon/syscalls/object.h>
#include <zircon/types.h>

#include <algorithm>
#include <iterator>
#include <string>

#include "zircon/system/ulib/profile/config.h"

namespace {

constexpr char kConfigPath[] = "/config/profiles";

using zircon_profile::MaybeMediaRole;
using zircon_profile::ParseRoleSelector;
using zircon_profile::ProfileMap;

class ProfileProvider : public fidl::WireServer<fuchsia_scheduler::ProfileProvider> {
 public:
  static zx::result<ProfileProvider*> Create(const zx::job& root_job);

  void OnConnect(async_dispatcher_t* dispatcher,
                 fidl::ServerEnd<fuchsia_scheduler::ProfileProvider> server_end) {
    bindings_.AddBinding(dispatcher, std::move(server_end), this, fidl::kIgnoreBindingClosure);
  }

 private:
  ProfileProvider(const zx::job& root_job, ProfileMap profiles)
      : root_job_(root_job), profiles_(std::move(profiles)) {}

  void GetProfile(GetProfileRequestView request, GetProfileCompleter::Sync& completer) override;

  void GetDeadlineProfile(GetDeadlineProfileRequestView request,
                          GetDeadlineProfileCompleter::Sync& completer) override;

  void GetCpuAffinityProfile(GetCpuAffinityProfileRequestView request,
                             GetCpuAffinityProfileCompleter::Sync& completer) override;

  void SetProfileByRole(SetProfileByRoleRequestView request,
                        SetProfileByRoleCompleter::Sync& completer) override;

  fidl::ServerBindingGroup<fuchsia_scheduler::ProfileProvider> bindings_;
  zx::unowned_job root_job_;
  ProfileMap profiles_;
};

void ProfileProvider::GetProfile(GetProfileRequestView request,
                                 GetProfileCompleter::Sync& completer) {
  const std::string name{request->name.get()};
  FX_SLOG(INFO, "Priority requested", KV("name", name.c_str()), KV("priority", request->priority),
          KV("tag", "ProfileProvider"));

  zx_profile_info_t info = {
      .flags = ZX_PROFILE_INFO_FLAG_PRIORITY,
      .priority = static_cast<int32_t>(std::min<uint32_t>(
          std::max<uint32_t>(request->priority, ZX_PRIORITY_LOWEST), ZX_PRIORITY_HIGHEST)),
  };

  zx::profile profile;
  zx_status_t status = zx::profile::create(*root_job_, 0u, &info, &profile);
  completer.Reply(status, std::move(profile));
}

void ProfileProvider::GetDeadlineProfile(GetDeadlineProfileRequestView request,
                                         GetDeadlineProfileCompleter::Sync& completer) {
  const std::string name{request->name.get()};
  const double utilization =
      static_cast<double>(request->capacity) / static_cast<double>(request->deadline);
  FX_SLOG(INFO, "Deadline requested", KV("name", name.c_str()), KV("capacity", request->capacity),
          KV("deadline", request->deadline), KV("period", request->period),
          KV("utilization", utilization), KV("tag", "ProfileProvider"));

  zx_profile_info_t info = {
      .flags = ZX_PROFILE_INFO_FLAG_DEADLINE,
      .deadline_params =
          zx_sched_deadline_params_t{
              .capacity = static_cast<zx_duration_t>(request->capacity),
              .relative_deadline = static_cast<zx_duration_t>(request->deadline),
              .period = static_cast<zx_duration_t>(request->period),
          },
  };

  zx::profile profile;
  zx_status_t status = zx::profile::create(*root_job_, 0u, &info, &profile);
  completer.Reply(status, std::move(profile));
}

void ProfileProvider::GetCpuAffinityProfile(GetCpuAffinityProfileRequestView request,
                                            GetCpuAffinityProfileCompleter::Sync& completer) {
  zx_profile_info_t info = {
      .flags = ZX_PROFILE_INFO_FLAG_CPU_MASK,
  };

  static_assert(sizeof(info.cpu_affinity_mask.mask) == sizeof(request->cpu_mask.mask));
  static_assert(std::size(info.cpu_affinity_mask.mask) ==
                std::size(decltype(request->cpu_mask.mask){}));
  memcpy(info.cpu_affinity_mask.mask, request->cpu_mask.mask.begin(),
         sizeof(request->cpu_mask.mask));

  zx::profile profile;
  zx_status_t status = zx::profile::create(*root_job_, 0u, &info, &profile);
  completer.Reply(status, std::move(profile));
}

void ProfileProvider::SetProfileByRole(SetProfileByRoleRequestView request,
                                       SetProfileByRoleCompleter::Sync& completer) {
  // Log the requested role and PID:TID of the thread being assigned.
  zx_info_handle_basic_t handle_info{};
  zx_status_t status = request->thread.get_info(ZX_INFO_HANDLE_BASIC, &handle_info,
                                                sizeof(handle_info), nullptr, nullptr);
  if (status != ZX_OK) {
    FX_SLOG(WARNING, "Failed to get info for thread handle",
            KV("status", zx_status_get_string(status)));
    handle_info.koid = ZX_KOID_INVALID;
    handle_info.related_koid = ZX_KOID_INVALID;
  }

  const std::string role_selector{request->role.get()};
  FX_SLOG(DEBUG, "Role requested", KV("ProfileProvider", role_selector.c_str()),
          KV("pid", handle_info.related_koid), KV("tid", handle_info.koid),
          KV("tag", "ProfileProvider"));

  const fit::result role_result = ParseRoleSelector(role_selector);
  if (role_result.is_error()) {
    return completer.Reply(ZX_ERR_INVALID_ARGS);
  }

  // Select the profile parameters based on the role selector. The builtin roles cannot be
  // overridden.
  zx_profile_info_t info = {};
  if (role_result->name == "fuchsia.default") {
    info.flags = ZX_PROFILE_INFO_FLAG_PRIORITY;
    info.priority = ZX_PRIORITY_DEFAULT;
  } else if (role_result->name == "fuchsia.test-role" && role_result->has("not-found")) {
    return completer.Reply(ZX_ERR_NOT_FOUND);
  } else if (role_result->name == "fuchsia.test-role" && role_result->has("ok")) {
    return completer.Reply(ZX_OK);
  } else if (auto search = profiles_.find(role_result->name); search != profiles_.cend()) {
    info = search->second.info;
  } else if (const auto media_role = MaybeMediaRole(*role_result); media_role.is_ok()) {
    // TODO(fxbug.dev/40858): If a media profile is not found in the system config, use the
    // forwarded parameters. This can be removed once clients are migrated to use defined roles.
    // Skip media roles with invalid deadline parameters.
    if (media_role->capacity <= 0 || media_role->deadline <= 0 ||
        media_role->capacity > media_role->deadline) {
      FX_SLOG(WARNING, "Skipping media profile with no override and invalid selectors",
              KV("capacity", media_role->capacity), KV("deadline", media_role->deadline),
              KV("role", role_result->name.c_str()), KV("tag", "ProfileProvider"));
      return completer.Reply(ZX_OK);
    }

    FX_SLOG(INFO, "Using selector parameters for media profile with no override",
            KV("capacity", media_role->capacity), KV("deadline", media_role->deadline),
            KV("role", role_result->name.c_str()), KV("tag", "ProfileProvider"));

    info.flags = ZX_PROFILE_INFO_FLAG_DEADLINE;
    info.deadline_params.capacity = media_role->capacity;
    info.deadline_params.relative_deadline = media_role->deadline;
    info.deadline_params.period = media_role->deadline;
  } else {
    FX_SLOG(DEBUG, "Requested role not found", KV("role", role_result->name.c_str()),
            KV("tag", "ProfileProvider"));
    return completer.Reply(ZX_ERR_NOT_FOUND);
  }

  zx::profile profile;
  status = zx::profile::create(*root_job_, 0u, &info, &profile);
  if (status != ZX_OK) {
    // Failing to create a profile is likely due to invalid profile parameters.
    return completer.Reply(ZX_ERR_INTERNAL);
  }

  status = request->thread.set_profile(profile, 0);
  completer.Reply(status);
}

constexpr const char* profile_svc_names[] = {
    fidl::DiscoverableProtocolName<fuchsia_scheduler::ProfileProvider>,
    nullptr,
};

zx::result<ProfileProvider*> ProfileProvider::Create(const zx::job& root_job) {
  auto result = zircon_profile::LoadConfigs(kConfigPath);
  if (result.is_error()) {
    FX_SLOG(ERROR, "Failed to load configs", KV("error", result.error_value().c_str()),
            KV("tag", "ProfileProvider"));
    return zx::error(ZX_ERR_INTERNAL);
  }

  // Apply the dispatch role if defined.
  const std::string dispatch_role = "fuchsia.system.profile-provider.dispatch";
  const auto search = result->find(dispatch_role);
  if (search != result->end()) {
    zx::profile profile;
    zx_status_t status = zx::profile::create(root_job, 0u, &search->second.info, &profile);
    if (status != ZX_OK) {
      FX_SLOG(ERROR, "Failed to create profile for role", KV("role", dispatch_role.c_str()),
              KV("error", zx_status_get_string(status)), KV("tag", "ProfileProvider"));
    } else {
      status = zx_object_set_profile(zx_thread_self(), profile.get(), 0);
      if (status != ZX_OK) {
        FX_SLOG(ERROR, "Failed to set profile", KV("error", zx_status_get_string(status)),
                KV("tag", "ProfileProvider"));
      }
    }
  }

  return zx::ok(new ProfileProvider{root_job, std::move(result.value())});
}

zx_status_t init(void** out_ctx) {
  const auto root_job =
      zx::unowned_job{static_cast<zx_handle_t>(reinterpret_cast<uintptr_t>(*out_ctx))};
  zx::result provider = ProfileProvider::Create(*root_job);
  if (!provider.is_ok()) {
    return provider.status_value();
  }
  *out_ctx = provider.value();
  return ZX_OK;
}

zx_status_t connect(void* ctx, async_dispatcher_t* dispatcher, const char* service_name,
                    zx_handle_t request) {
  if (std::string_view{service_name} ==
      fidl::DiscoverableProtocolName<fuchsia_scheduler::ProfileProvider>) {
    static_cast<ProfileProvider*>(ctx)->OnConnect(
        dispatcher, fidl::ServerEnd<fuchsia_scheduler::ProfileProvider>{zx::channel{request}});
    return ZX_OK;
  }

  zx_handle_close(request);
  return ZX_ERR_NOT_SUPPORTED;
}

constexpr zx_service_ops_t service_ops = {
    .init = init,
    .connect = connect,
    .release = nullptr,
};

constexpr zx_service_provider_t profile_service_provider = {
    .version = SERVICE_PROVIDER_VERSION,
    .services = profile_svc_names,
    .ops = &service_ops,
};

}  // namespace

const zx_service_provider_t* profile_get_service_provider() { return &profile_service_provider; }
