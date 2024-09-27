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

class WakeLease {
 public:
  // If |log| is set to true, logs will be emitted when acquiring leases and when lease times out.
  // An invalid |sag_client| will result in silently disabling wake lease acquisition.
  WakeLease(async_dispatcher_t* dispatcher, std::string_view lease_name,
            fidl::ClientEnd<fuchsia_power_system::ActivityGovernor> sag_client,
            inspect::Node* parent_node = nullptr, bool log = false);

  // Acquire a wake lease and automatically drop it after the specified timeout. If a lease was
  // still held from an earlier invocation, it will be extended until the new timeout.
  // Note that a duration is taken becasue the deadline is computed once the lease is acquired,
  // rather than at the point this method is called.
  bool AcquireWakeLease(zx::duration timeout);

  // Cancel timeout and take the wake lease.
  // Note that it's possible for the wake lease to not be valid, so the caller should check it's
  // validity before using.
  zx::eventpair TakeWakeLease();

 private:
  void HandleTimeout();

  async_dispatcher_t* dispatcher_;
  std::string lease_name_;
  bool log_;
  fidl::WireSyncClient<fuchsia_power_system::ActivityGovernor> sag_client_;

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
