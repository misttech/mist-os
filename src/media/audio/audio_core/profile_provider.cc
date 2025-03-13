// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/audio_core/profile_provider.h"

#include <lib/syslog/cpp/macros.h>

#include <sstream>

#include "src/media/audio/audio_core/mix_profile_config.h"

namespace media::audio {

fidl::InterfaceRequestHandler<fuchsia::media::ProfileProvider>
ProfileProvider::GetFidlRequestHandler() {
  return bindings_.GetHandler(this);
}

void ProfileProvider::RegisterHandlerWithCapacity(zx::thread thread_handle,
                                                  const std::string role_name, int64_t period,
                                                  float utilization,
                                                  RegisterHandlerWithCapacityCallback callback) {
  if (!role_manager_) {
    role_manager_ = context_.svc()->Connect<fuchsia::scheduler::RoleManager>();
  }

  const zx::duration interval = period ? zx::duration(period) : mix_profile_period_;
  const float scaled_interval = static_cast<float>(interval.to_nsecs()) * utilization;
  const zx::duration capacity(static_cast<zx_duration_t>(scaled_interval));

  auto request = std::move(
      fuchsia::scheduler::RoleManagerSetRoleRequest()
          .set_target(fuchsia::scheduler::RoleTarget::WithThread(std::move(thread_handle)))
          .set_role(fuchsia::scheduler::RoleName{role_name}));

  role_manager_->SetRole(std::move(request),
                         [interval, capacity, callback = std::move(callback),
                          role_name](fuchsia::scheduler::RoleManager_SetRole_Result result) {
                           if (result.is_response()) {
                             callback(interval.get(), capacity.get());
                           } else if (result.is_err()) {
                             FX_PLOGS(WARNING, result.err())
                                 << "Failed to set role \"" << role_name << "\" for thread";
                             callback(0, 0);
                           } else {
                             // This case should never happen, as it can only happen on an unknown
                             // method call or an invalid fidl message tag.
                             callback(0, 0);
                           }
                         });
}

void ProfileProvider::UnregisterHandler(zx::thread thread_handle, const std::string name,
                                        UnregisterHandlerCallback callback) {
  if (!role_manager_) {
    role_manager_ = context_.svc()->Connect<fuchsia::scheduler::RoleManager>();
  }

  const std::string role_name = "fuchsia.default";
  auto request = std::move(
      fuchsia::scheduler::RoleManagerSetRoleRequest()
          .set_target(fuchsia::scheduler::RoleTarget::WithThread(std::move(thread_handle)))
          .set_role(fuchsia::scheduler::RoleName{role_name}));

  role_manager_->SetRole(
      std::move(request), [callback = std::move(callback), &role_name,
                           &name](fuchsia::scheduler::RoleManager_SetRole_Result result) {
        if (result.is_err()) {
          FX_PLOGS(WARNING, result.err())
              << "Failed to set role \"" << role_name << "\" for thread \"" << name << "\"";
        }
        callback();
      });
}

void ProfileProvider::RegisterMemoryRange(zx::vmar vmar_handle, std::string name,
                                          RegisterMemoryRangeCallback callback) {
  if (!role_manager_) {
    role_manager_ = context_.svc()->Connect<fuchsia::scheduler::RoleManager>();
  }

  auto request =
      std::move(fuchsia::scheduler::RoleManagerSetRoleRequest()
                    .set_target(fuchsia::scheduler::RoleTarget::WithVmar(std::move(vmar_handle)))
                    .set_role(fuchsia::scheduler::RoleName{name}));

  role_manager_->SetRole(
      std::move(request), [callback = std::move(callback),
                           &name](fuchsia::scheduler::RoleManager_SetRole_Result result) {
        if (result.is_err()) {
          FX_PLOGS(WARNING, result.err()) << "Failed to set memory role \"" << name << "\"";
        }
        callback();
      });
}

void ProfileProvider::UnregisterMemoryRange(zx::vmar vmar_handle,
                                            UnregisterMemoryRangeCallback callback) {
  if (!role_manager_) {
    role_manager_ = context_.svc()->Connect<fuchsia::scheduler::RoleManager>();
  }

  const std::string role_name = "fuchsia.default";
  auto request =
      std::move(fuchsia::scheduler::RoleManagerSetRoleRequest()
                    .set_target(fuchsia::scheduler::RoleTarget::WithVmar(std::move(vmar_handle)))
                    .set_role(fuchsia::scheduler::RoleName{role_name}));

  role_manager_->SetRole(
      std::move(request), [callback = std::move(callback),
                           &role_name](fuchsia::scheduler::RoleManager_SetRole_Result result) {
        if (result.is_err()) {
          FX_PLOGS(WARNING, result.err()) << "Failed to set memory role \"" << role_name << "\"";
        }
        callback();
      });
}

}  // namespace media::audio
