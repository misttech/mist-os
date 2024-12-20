// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_POWER_CPP_WAKE_LEASE_H_
#define LIB_DRIVER_POWER_CPP_WAKE_LEASE_H_

#include <fidl/fuchsia.power.system/cpp/wire.h>
#include <lib/async/cpp/task.h>
#include <lib/inspect/cpp/vmo/types.h>
#include <lib/zx/eventpair.h>
#include <lib/zx/result.h>

#include <string>

#if FUCHSIA_API_LEVEL_AT_LEAST(HEAD)

namespace fdf_power {

class WakeLease : public fidl::WireServer<fuchsia_power_system::ActivityGovernorListener> {
 public:
  // If |log| is set to true, logs will be emitted when acquiring leases and when lease times out.
  // An invalid |sag_client| will result in silently disabling wake lease acquisition.
  WakeLease(async_dispatcher_t* dispatcher, std::string_view lease_name,
            fidl::ClientEnd<fuchsia_power_system::ActivityGovernor> sag_client,
            inspect::Node* parent_node = nullptr, bool log = false);

  // Ensure that the system stays awake until the timeout is reached. To accomplish this we either:
  //   * obtain a wake lease immediately if the system is currently suspended
  //   * obtain a wake lease in the future if the system begins suspension before the timeout is
  //     reached
  // If a wake lease is acquired, it is dropped when the timeout is reached. The timeout may be
  // extended by additional calls to `HandleInterrupt`. If the system is not suspended and does not
  // attempt suspension during the timeout period, no wake lease is obtained.
  //
  // Ideally this is only called after we wake up due to an interrupt. Note that a duration is
  // taken because the deadline is computed once the lease is acquired, rather than at the point
  // this method is called.
  bool HandleInterrupt(zx::duration timeout);

  // Acquire a wake lease and automatically drop it after the specified timeout. If a lease was
  // still held from an earlier invocation, it will be extended until the new timeout.
  // Note that a duration is taken because the deadline is computed once the lease is acquired,
  // rather than at the point this method is called.
  bool AcquireWakeLease(zx::duration timeout);

  // Deposit a wake lease which will automatically be dropped after the specified timeout deadline.
  // If a lease was already held from an earlier invocation, it will be dropped in favor of the new
  // lease if the new lease has a later deadline. If the old lease has a later deadline, then the
  // new lease will be dropped instead.
  void DepositWakeLease(zx::eventpair wake_lease, zx::time timeout_deadline);

  // Cancel timeout and take the wake lease. Returns ZX_ERR_BAD_HANDLE if we don't currenly have a
  // wake lease.
  zx::result<zx::eventpair> TakeWakeLease();

  // Get a duplicate of the stored wake lease. Returns ZX_ERR_BAD_HANDLE if we don't currently have
  // a wake lease.
  zx::result<zx::eventpair> GetWakeLeaseCopy();

  // fuchsia.power.system/ActivityGovernorListener implementation.
  void OnResume(OnResumeCompleter::Sync& completer) override;
  void OnSuspendStarted(OnSuspendStartedCompleter::Sync& completer) override;
  void OnSuspendFail(OnSuspendFailCompleter::Sync& completer) override;
  void handle_unknown_method(
      fidl::UnknownMethodMetadata<fuchsia_power_system::ActivityGovernorListener> metadata,
      fidl::UnknownMethodCompleter::Sync& completer) override;

 private:
  void HandleTimeout();

  void ResetSagClient();

  async_dispatcher_t* dispatcher_;
  std::string lease_name_;
  bool log_;
  fidl::WireSyncClient<fuchsia_power_system::ActivityGovernor> sag_client_;
  std::optional<fidl::ServerBinding<fuchsia_power_system::ActivityGovernorListener>>
      listener_binding_;
  bool system_suspended_ = false;
  // Time before which we should take a wake lease if we are informed of
  // suspension.
  zx_time_t prevent_sleep_before_ = 0;

  async::TaskClosureMethod<WakeLease, &WakeLease::HandleTimeout> lease_task_{this};
  zx::eventpair lease_;

  inspect::UintProperty total_lease_acquisitions_;
  inspect::BoolProperty wake_lease_held_;
  inspect::BoolProperty wake_lease_grabbable_;
  inspect::UintProperty wake_lease_last_acquired_timestamp_;
  inspect::UintProperty wake_lease_last_refreshed_timestamp_;
};

}  // namespace fdf_power

#endif

#endif  // LIB_DRIVER_POWER_CPP_WAKE_LEASE_H_
