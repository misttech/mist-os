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

// Wrapper around usage of fuchsia.power.system/ActivityGovernor.AcquireWakeLease. The wrapper
// reduces wake lease creation by allow callers to set timeout after which to drop the lease and
// provides mechanisms to extend that timeout.
class WakeLease : public fidl::WireServer<fuchsia_power_system::ActivityGovernorListener> {
 public:
  // If |log| is set to true, logs will be emitted when acquiring leases and when lease times out.
  // An invalid |sag_client| will result in silently disabling wake lease acquisition.
  WakeLease(async_dispatcher_t* dispatcher, std::string_view lease_name,
            fidl::ClientEnd<fuchsia_power_system::ActivityGovernor> sag_client,
            inspect::Node* parent_node = nullptr, bool log = false);

  // Acquire a wake lease **only** if the system is in a suspended state. Ideally this is only
  // called after we wake up due to an interrupt, however it may be called due to an interrupt
  // firing after the suspension process begins. If acquired, the wake lease will be dropped after
  // the specified timeout. If a lease was still held from an earlier invocation, it will be
  // extended until the new timeout. Note that a duration is taken because the deadline is
  // computed once the lease is acquired, rather than at the point this method is called.
  bool HandleInterrupt(zx::duration timeout);

  // Acquire a wake lease and automatically drop it after the specified timeout. If a lease was
  // still held from an earlier invocation, it will be extended until the new timeout.
  // Note that a duration is taken because the deadline is computed once the lease is acquired,
  // rather than at the point this method is called.
  bool AcquireWakeLease(zx::duration timeout);

  // Provide a wake lease which will be dropped either:
  //   * immediately if there is already a wake lease with a later deadline
  //   * at the specified deadline
  // In the latter case any previous lease held by this object is dropped immediately.
  void DepositWakeLease(zx::eventpair wake_lease, zx::time timeout_deadline);

  // Cancel timeout and take the wake lease. Returns ZX_ERR_BAD_HANDLE if we don't currently have a
  // wake lease.
  zx::result<zx::eventpair> TakeWakeLease();

  // Get a duplicate of the stored wake lease. Returns ZX_ERR_BAD_HANDLE if we don't currently have
  // a wake lease.
  zx::result<zx::eventpair> GetWakeLeaseCopy();

  // fuchsia.power.system/ActivityGovernorListener implementation. This is used to avoid creating
  // wake leases in cases where the system is resumed and `HandleInterrupt` is called.
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
  bool system_suspended_ = true;

  async::TaskClosureMethod<WakeLease, &WakeLease::HandleTimeout> lease_task_{this};
  zx::eventpair lease_;

  inspect::UintProperty total_lease_acquisitions_;
  inspect::BoolProperty wake_lease_held_;
  inspect::BoolProperty wake_lease_grabbable_;
  inspect::UintProperty wake_lease_last_attempted_acquisition_timestamp_;
  inspect::UintProperty wake_lease_last_acquired_timestamp_;
  inspect::UintProperty wake_lease_last_refreshed_timestamp_;
};

}  // namespace fdf_power

#endif

#endif  // LIB_DRIVER_POWER_CPP_WAKE_LEASE_H_
