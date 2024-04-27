// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.device.manager/cpp/wire.h>
#include <fidl/fuchsia.hardware.power.statecontrol/cpp/wire.h>
#include <fidl/fuchsia.io/cpp/wire.h>
#include <fidl/fuchsia.power.broker/cpp/wire.h>
#include <fidl/fuchsia.power.system/cpp/wire.h>
#include <fidl/fuchsia.process.lifecycle/cpp/wire.h>
#include <fidl/fuchsia.sys2/cpp/wire.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/fit/function.h>
#include <zircon/errors.h>
#include <zircon/process.h>
#include <zircon/processargs.h>
#include <zircon/status.h>

#include <chrono>
#include <optional>
#include <thread>

#include <fbl/auto_lock.h>
#include <fbl/mutex.h>
#include <fbl/string_printf.h>

#include "src/storage/lib/vfs/cpp/managed_vfs.h"
#include "src/storage/lib/vfs/cpp/pseudo_dir.h"
#include "src/storage/lib/vfs/cpp/service.h"
#include "src/sys/lib/stdout-to-debuglog/cpp/stdout-to-debuglog.h"

namespace fio = fuchsia_io;

namespace statecontrol_fidl = fuchsia_hardware_power_statecontrol;
namespace sys2_fidl = fuchsia_sys2;
namespace broker_fidl = fuchsia_power_broker;
namespace system_fidl = fuchsia_power_system;

// The amount of time that the shim will spend trying to connect to
// power_manager before giving up.
// TODO(https://fxbug.dev/42131944): increase this timeout
const zx::duration SERVICE_CONNECTION_TIMEOUT = zx::sec(2);

// The amount of time that the shim will spend waiting for a manually trigger
// system shutdown to finish before forcefully restarting the system.
const std::chrono::duration MANUAL_SYSTEM_SHUTDOWN_TIMEOUT = std::chrono::minutes(60);

// The valid power levels for the shutdown_control power element.
enum class ShutdownControlLevel : uint8_t {
  // The shutdown_control power element is not active.
  // System suspension is allowed.
  kInactive = 0u,
  // The shutdown_control power element is active.
  // System suspension is not allowed.
  kActive = 1u
};

class SystemStateTransitionServer final
    : public fidl::WireServer<fuchsia_device_manager::SystemStateTransition> {
 public:
  SystemStateTransitionServer() = default;

  // fuchsia.device.manager/SystemStateTransition APIs.
  void GetTerminationSystemState(GetTerminationSystemStateCompleter::Sync& completer) override;
  void GetMexecZbis(GetMexecZbisCompleter::Sync& completer) override;

  void set_system_power_state(fuchsia_device_manager::SystemPowerState system_power_state) {
    fbl::AutoLock al(&lock_);
    system_power_state_ = system_power_state;
  }

  void set_mexec_kernel_zbi(zx::vmo kernel_zbi) {
    fbl::AutoLock al(&lock_);
    mexec_kernel_zbi_ = std::move(kernel_zbi);
  }

  void set_mexec_data_zbi(zx::vmo data_zbi) {
    fbl::AutoLock al(&lock_);
    mexec_data_zbi_ = std::move(data_zbi);
  }

 private:
  fbl::Mutex lock_;
  fuchsia_device_manager::SystemPowerState system_power_state_ __TA_GUARDED(&lock_) =
      fuchsia_device_manager::SystemPowerState::kFullyOn;
  zx::vmo mexec_kernel_zbi_ __TA_GUARDED(&lock_);
  zx::vmo mexec_data_zbi_ __TA_GUARDED(&lock_);
};

class StateControlAdminServer final : public fidl::WireServer<statecontrol_fidl::Admin> {
 public:
  StateControlAdminServer() : loop_((&kAsyncLoopConfigNoAttachToCurrentThread)) {
    loop_.StartThread("SystemStateTransitionLoop");

    // Set up a power element that keeps the system awake during reboots and shutdowns.
    // The new power element, shutdown_control, has an active dependency on
    // SAG's WakeHandling power element when its power level is
    // ShutdownControlLevel::kActive.

    // Start by getting a dependency token to system activity governor's WakeHandling power element.
    zx::result activity_governor = component::Connect<system_fidl::ActivityGovernor>();
    if (activity_governor.is_error()) {
      fprintf(stderr, "[shutdown-shim]: error connecting to system_activity_governor: %s\n",
              activity_governor.status_string());
      return;
    }

    fidl::WireSyncClient activity_governor_client{std::move(activity_governor.value())};
    auto get_resp = activity_governor_client->GetPowerElements();
    if (get_resp.status() != ZX_OK) {
      fprintf(stderr, "[shutdown-shim]: error getting activity governor power elements: %s\n",
              get_resp.status_string());
      return;
    }

    if (!get_resp.value().has_wake_handling()) {
      fprintf(stderr, "[shutdown-shim]: wake_handling power element not available\n");
      return;
    }

    auto wake_handling = std::move(get_resp.value().wake_handling());
    if (!wake_handling.has_active_dependency_token()) {
      fprintf(stderr, "[shutdown-shim]: wake_handling dependency token not available\n");
      return;
    }

    // Next, create the new power element with power-broker.
    zx::result topology = component::Connect<broker_fidl::Topology>();
    if (topology.is_error()) {
      fprintf(stderr, "[shutdown-shim]: error connecting to power_broker: %s\n",
              topology.status_string());
      return;
    }

    fidl::WireSyncClient topology_client{std::move(topology.value())};

    // Use the WakeHandling dependency token to set up an active dependency
    // between shutdown-shim's new power element and system-activity-governor's
    // WakeHandling power element.
    auto requires_token = std::move(wake_handling.active_dependency_token());
    auto wake_handling_dep = broker_fidl::wire::LevelDependency{
        .dependency_type = broker_fidl::wire::DependencyType::kActive,
        .dependent_level = static_cast<uint8_t>(ShutdownControlLevel::kActive),
        .requires_token = std::move(requires_token),
        .requires_level = static_cast<uint8_t>(system_fidl::wire::WakeHandlingLevel::kActive),
    };
    std::vector<broker_fidl::wire::LevelDependency> dependencies;
    dependencies.push_back(std::move(wake_handling_dep));

    auto lessor_endpoints = fidl::CreateEndpoints<broker_fidl::Lessor>();
    if (!lessor_endpoints.is_ok()) {
      fprintf(stderr, "[shutdown-shim]: error creating Lessor endpoints: %s\n",
              lessor_endpoints.status_string());
      return;
    }

    std::vector<uint8_t> valid_levels{
        static_cast<uint8_t>(ShutdownControlLevel::kInactive),
        static_cast<uint8_t>(ShutdownControlLevel::kActive),
    };

    fidl::Arena arena;
    auto shutdown_control_schema =
        broker_fidl::wire::ElementSchema::Builder(arena)
            .element_name(fidl::StringView::FromExternal("shutdown_control"))
            .valid_levels(fidl::VectorView<uint8_t>::FromExternal(valid_levels))
            .initial_current_level(static_cast<uint8_t>(ShutdownControlLevel::kInactive))
            .dependencies(
                fidl::VectorView<broker_fidl::wire::LevelDependency>::FromExternal(dependencies))
            .lessor_channel(std::move(lessor_endpoints->server))
            .Build();

    auto resp = topology_client->AddElement(std::move(shutdown_control_schema));
    if (resp.status() != ZX_OK) {
      fprintf(stderr, "[shutdown-shim]: error requesting shutdown_control power element: %s\n",
              resp.status_string());
      return;
    }
    if (resp.value().is_error()) {
      fprintf(stderr, "[shutdown-shim]: error creating shutdown_control power element: %u\n",
              static_cast<uint32_t>(resp.value().error_value()));
      return;
    }

    element_control_channel_client_end_ = std::move(resp.value().value()->element_control_channel);
    lessor_client_ = fidl::WireSyncClient{std::move(lessor_endpoints->client)};
  }

  zx_status_t ExportServices(fbl::RefPtr<fs::PseudoDir>& svc_dir, async_dispatcher* dispatcher);

  // fuchsia.hardware.power.statecontrol/Admin APIs.
  void PowerFullyOn(PowerFullyOnCompleter::Sync& completer) override;
  void Reboot(RebootRequestView request, RebootCompleter::Sync& completer) override;
  void RebootToBootloader(RebootToBootloaderCompleter::Sync& completer) override;
  void RebootToRecovery(RebootToRecoveryCompleter::Sync& completer) override;
  void Poweroff(PoweroffCompleter::Sync& completer) override;
  void Mexec(MexecRequestView request, MexecCompleter::Sync& completer) override;
  void SuspendToRam(SuspendToRamCompleter::Sync& completer) override;

 private:
  std::optional<fidl::WireSyncClient<broker_fidl::LeaseControl>> AcquireShutdownControlLease()
      const;

  SystemStateTransitionServer system_state_transition_server_;
  async::Loop loop_;
  fidl::ClientEnd<broker_fidl::ElementControl> element_control_channel_client_end_;
  fidl::WireSyncClient<broker_fidl::Lessor> lessor_client_;
};

// Opens a service node, failing if the provider of the service does not respond
// to messages within SERVICE_CONNECTION_TIMEOUT.
//
// This is accomplished by opening the service node, writing an invalid message
// to the channel, and observing PEER_CLOSED within the timeout. This is testing
// that something is responding to open requests for this service, as opposed to
// the intended provider for this service being stuck on component resolution
// indefinitely, which causes connection attempts to the component to never
// succeed nor fail. By observing a PEER_CLOSED, we can ensure that the service
// provider received our message and threw it out (or the provider doesn't
// exist). Upon receiving the PEER_CLOSED, we then open a new connection and
// save it in `local`.
//
// This is protecting against packaged components being stuck in resolution for
// forever, which happens if pkgfs never starts up (this always happens on
// bringup). Once a component is able to be resolved, then all new service
// connections will either succeed or fail rather quickly.
template <typename Protocol>
zx::result<fidl::ClientEnd<Protocol>> connect_to_protocol_with_timeout(
    const char* name = fidl::DiscoverableProtocolDefaultPath<Protocol>) {
  zx::result channel = component::Connect<Protocol>(name);
  if (channel.is_error()) {
    return channel.take_error();
  }

  // We want to use the zx_channel_call syscall directly here, because there's
  // no way to set the timeout field on the call using the FIDL bindings.
  char garbage_data[6] = {0, 1, 2, 3, 4, 5};
  zx_channel_call_args_t call = {
      // Bytes to send in the channel call
      .wr_bytes = garbage_data,
      // Handles to send in the channel call
      .wr_handles = nullptr,
      // Buffer to write received bytes into from the channel call
      .rd_bytes = nullptr,
      // Buffer to write received handles into from the channel call
      .rd_handles = nullptr,
      // Number of bytes to send
      .wr_num_bytes = 6,
      // Number of bytes we can receive
      .rd_num_bytes = 0,
      // Number of handles we can receive
      .rd_num_handles = 0,
  };
  uint32_t actual_bytes, actual_handles;
  switch (zx_status_t status =
              channel.value().channel().call(0, zx::deadline_after(SERVICE_CONNECTION_TIMEOUT),
                                             &call, &actual_bytes, &actual_handles);
          status) {
    case ZX_ERR_TIMED_OUT:
      fprintf(stderr, "[shutdown-shim]: timed out connecting to %s\n", name);
      return zx::error(status);
    case ZX_ERR_PEER_CLOSED:
      return component::Connect<Protocol>(name);
    default:
      fprintf(stderr, "[shutdown-shim]: unexpected response from %s: %s\n", name,
              zx_status_get_string(status));
      return zx::error(status);
  }
}

// Connect to fuchsia.sys2.SystemController and initiate a system shutdown. If
// everything goes well, this function shouldn't return until shutdown is
// complete.
zx_status_t initiate_component_shutdown() {
  zx::result local = component::Connect<sys2_fidl::SystemController>();
  if (local.is_error()) {
    fprintf(stderr, "[shutdown-shim]: error connecting to component_manager: %s\n",
            local.status_string());
    return local.error_value();
  }
  fidl::WireSyncClient system_controller_client{std::move(local.value())};

  printf("[shutdown-shim]: calling system_controller_client.Shutdown()\n");
  auto resp = system_controller_client->Shutdown();
  printf("[shutdown-shim]: status was returned: %s\n", resp.status_string());
  return resp.status();
}

// Sleeps for MANUAL_SYSTEM_SHUTDOWN_TIMEOUT, and then exits the process
void shutdown_timer() {
  std::this_thread::sleep_for(MANUAL_SYSTEM_SHUTDOWN_TIMEOUT);

  // We shouldn't still be running at this point

  exit(1);
}

// Manually drive a shutdown by setting state as driver_manager's termination
// behavior and then instructing component_manager to perform an orderly
// shutdown of components. If the orderly shutdown takes too long the shim will
// exit with a non-zero exit code, killing the root job.
void drive_shutdown_manually(fuchsia_device_manager::SystemPowerState state) {
  fprintf(stderr, "[shutdown-shim]: driving shutdown manually\n");

  // Start a new thread that makes us exit uncleanly after a timeout. This will
  // guarantee that shutdown doesn't take longer than
  // MANUAL_SYSTEM_SHUTDOWN_TIMEOUT, because we're marked as critical to the
  // root job and us exiting will bring down userspace and cause a reboot.
  std::thread(shutdown_timer).detach();

  zx_status_t status = initiate_component_shutdown();
  if (status != ZX_OK) {
    fprintf(
        stderr,
        "[shutdown-shim]: error initiating component shutdown, system shutdown impossible: %s\n",
        zx_status_get_string(status));
    // Recovery from this state is impossible. Exit with a non-zero exit code,
    // so our critical marking causes the system to forcefully restart.
    exit(1);
  }
  fprintf(stderr, "[shutdown-shim]: manual shutdown successfully initiated\n");
}

zx_status_t send_command(fidl::WireSyncClient<statecontrol_fidl::Admin> statecontrol_client,
                         fuchsia_device_manager::SystemPowerState fallback_state,
                         const statecontrol_fidl::wire::RebootReason* reboot_reason = nullptr,
                         StateControlAdminServer::MexecRequestView* mexec_request = nullptr) {
  switch (fallback_state) {
    case fuchsia_device_manager::SystemPowerState::kReboot: {
      if (reboot_reason == nullptr) {
        fprintf(stderr, "[shutdown-shim]: internal error, bad pointer to reason for reboot\n");
        return ZX_ERR_INTERNAL;
      }
      auto resp = statecontrol_client->Reboot(*reboot_reason);
      if (resp.status() != ZX_OK) {
        return ZX_ERR_UNAVAILABLE;
      }
      if (resp->is_error()) {
        return resp->error_value();
      }
      return ZX_OK;

    } break;
    case fuchsia_device_manager::SystemPowerState::kRebootKernelInitiated: {
      auto resp = statecontrol_client->Reboot(*reboot_reason);
      if (resp.status() != ZX_OK) {
        return ZX_ERR_UNAVAILABLE;
      }
      if (resp.value().is_error()) {
        return resp.value().error_value();
      }
      return ZX_OK;

    } break;
    case fuchsia_device_manager::SystemPowerState::kRebootBootloader: {
      auto resp = statecontrol_client->RebootToBootloader();
      if (resp.status() != ZX_OK) {
        return ZX_ERR_UNAVAILABLE;
      }
      if (resp->is_error()) {
        return resp->error_value();
      }
      return ZX_OK;

    } break;
    case fuchsia_device_manager::SystemPowerState::kRebootRecovery: {
      auto resp = statecontrol_client->RebootToRecovery();
      if (resp.status() != ZX_OK) {
        return ZX_ERR_UNAVAILABLE;
      }
      if (resp->is_error()) {
        return resp->error_value();
      }
      return ZX_OK;

    } break;
    case fuchsia_device_manager::SystemPowerState::kPoweroff: {
      auto resp = statecontrol_client->Poweroff();
      if (resp.status() != ZX_OK) {
        return ZX_ERR_UNAVAILABLE;
      }
      if (resp->is_error()) {
        return resp->error_value();
      }
      return ZX_OK;

    } break;
    case fuchsia_device_manager::SystemPowerState::kMexec: {
      if (mexec_request == nullptr) {
        fprintf(stderr, "[shutdown-shim]: internal error, bad pointer to reason for mexec\n");
        return ZX_ERR_INTERNAL;
      }
      auto resp = statecontrol_client->Mexec(std::move((*mexec_request)->kernel_zbi),
                                             std::move((*mexec_request)->data_zbi));
      if (resp.status() != ZX_OK) {
        return ZX_ERR_UNAVAILABLE;
      }
      if (resp->is_error()) {
        return resp->error_value();
      }
      return ZX_OK;

    } break;
    case fuchsia_device_manager::SystemPowerState::kSuspendRam: {
      auto resp = statecontrol_client->SuspendToRam();
      if (resp.status() != ZX_OK) {
        return ZX_ERR_UNAVAILABLE;
      }
      if (resp->is_error()) {
        return resp->error_value();
      }
      return ZX_OK;

    } break;
    default:
      return ZX_ERR_INTERNAL;
  }
}

// Connects to power_manager and passes a SyncClient to the given function. The
// function is expected to return an error if there was a transport-related
// issue talking to power_manager, in which case this program will talk to
// driver_manager and component_manager to drive shutdown manually.
zx_status_t forward_command(fuchsia_device_manager::SystemPowerState fallback_state,
                            const statecontrol_fidl::wire::RebootReason* reboot_reason = nullptr) {
  zx::result local = connect_to_protocol_with_timeout<statecontrol_fidl::Admin>();
  if (local.is_ok()) {
    fprintf(stderr, "[shutdown-shim]: forwarding command %d\n",
            static_cast<uint8_t>(fallback_state));
    zx_status_t status =
        send_command(fidl::WireSyncClient(std::move(local.value())), fallback_state, reboot_reason);
    if (status != ZX_ERR_UNAVAILABLE && status != ZX_ERR_NOT_SUPPORTED) {
      return status;
    }
    // Power manager may decide not to support suspend. We should respect that and not attempt to
    // suspend manually.
    if (fallback_state == fuchsia_device_manager::SystemPowerState::kSuspendRam) {
      return status;
    }
  }

  fprintf(stderr, "[shutdown-shim]: failed to forward command to power_manager: %s\n",
          local.status_string());

  drive_shutdown_manually(fallback_state);

  // We should block on fuchsia.sys.SystemController forever on this thread, if
  // it returns something has gone wrong.
  fprintf(stderr, "[shutdown-shim]: we shouldn't still be running, crashing the system\n");
  exit(1);
}

void StateControlAdminServer::PowerFullyOn(PowerFullyOnCompleter::Sync& completer) {
  completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
}

void StateControlAdminServer::Reboot(RebootRequestView request, RebootCompleter::Sync& completer) {
  auto reboot_control_lease = AcquireShutdownControlLease();
  fuchsia_device_manager::SystemPowerState target_state =
      fuchsia_device_manager::SystemPowerState::kReboot;
  if (request->reason == statecontrol_fidl::wire::RebootReason::kOutOfMemory) {
    target_state = fuchsia_device_manager::SystemPowerState::kRebootKernelInitiated;
  }
  system_state_transition_server_.set_system_power_state(target_state);
  zx_status_t status = forward_command(target_state, &request->reason);
  if (status == ZX_OK) {
    completer.ReplySuccess();
  } else {
    completer.ReplyError(status);
  }
}

void StateControlAdminServer::RebootToBootloader(RebootToBootloaderCompleter::Sync& completer) {
  auto reboot_control_lease = AcquireShutdownControlLease();
  system_state_transition_server_.set_system_power_state(
      fuchsia_device_manager::SystemPowerState::kRebootBootloader);
  zx_status_t status = forward_command(fuchsia_device_manager::SystemPowerState::kRebootBootloader);
  if (status == ZX_OK) {
    completer.ReplySuccess();
  } else {
    completer.ReplyError(status);
  }
}

void StateControlAdminServer::RebootToRecovery(RebootToRecoveryCompleter::Sync& completer) {
  auto reboot_control_lease = AcquireShutdownControlLease();
  system_state_transition_server_.set_system_power_state(
      fuchsia_device_manager::SystemPowerState::kRebootRecovery);
  zx_status_t status = forward_command(fuchsia_device_manager::SystemPowerState::kRebootRecovery);
  if (status == ZX_OK) {
    completer.ReplySuccess();
  } else {
    completer.ReplyError(status);
  }
}

void StateControlAdminServer::Poweroff(PoweroffCompleter::Sync& completer) {
  auto reboot_control_lease = AcquireShutdownControlLease();
  system_state_transition_server_.set_system_power_state(
      fuchsia_device_manager::SystemPowerState::kPoweroff);
  zx_status_t status = forward_command(fuchsia_device_manager::SystemPowerState::kPoweroff);
  if (status == ZX_OK) {
    completer.ReplySuccess();
  } else {
    completer.ReplyError(status);
  }
}

void StateControlAdminServer::Mexec(MexecRequestView request, MexecCompleter::Sync& completer) {
  auto reboot_control_lease = AcquireShutdownControlLease();
  // Duplicate the VMOs now, as forwarding the mexec request to power-manager
  // will consume them.
  zx::vmo kernel_zbi, data_zbi;
  if (zx_status_t status = request->kernel_zbi.duplicate(ZX_RIGHT_SAME_RIGHTS, &kernel_zbi);
      status != ZX_OK) {
    completer.ReplyError(status);
    return;
  }
  if (zx_status_t status = request->data_zbi.duplicate(ZX_RIGHT_SAME_RIGHTS, &data_zbi);
      status != ZX_OK) {
    completer.ReplyError(status);
    return;
  }

  system_state_transition_server_.set_system_power_state(
      fuchsia_device_manager::SystemPowerState::kMexec);
  system_state_transition_server_.set_mexec_kernel_zbi(std::move(kernel_zbi));
  system_state_transition_server_.set_mexec_data_zbi(std::move(data_zbi));

  zx::result local = connect_to_protocol_with_timeout<statecontrol_fidl::Admin>();
  if (local.is_ok()) {
    fprintf(stderr, "[shutdown-shim]: trying to forward Mexec command\n");
    zx_status_t status =
        send_command(fidl::WireSyncClient(std::move(local.value())),
                     fuchsia_device_manager::SystemPowerState::kMexec, nullptr, &request);
    if (status == ZX_OK) {
      completer.ReplySuccess();
      return;
    }
    if (status != ZX_ERR_UNAVAILABLE && status != ZX_ERR_NOT_SUPPORTED) {
      completer.ReplyError(status);
      return;
    }
    // Else, fallback logic.
  }

  fprintf(stderr, "[shutdown-shim]: failed to forward command to power_manager: %s\n",
          local.status_string());

  drive_shutdown_manually(fuchsia_device_manager::SystemPowerState::kMexec);

  // We should block on fuchsia.sys.SystemController forever on this thread, if
  // it returns something has gone wrong.
  fprintf(stderr, "[shutdown-shim]: we shouldn't still be running, crashing the system\n");
  exit(1);
}

void StateControlAdminServer::SuspendToRam(SuspendToRamCompleter::Sync& completer) {
  system_state_transition_server_.set_system_power_state(
      fuchsia_device_manager::SystemPowerState::kSuspendRam);
  zx_status_t status = forward_command(fuchsia_device_manager::SystemPowerState::kSuspendRam);
  if (status == ZX_OK) {
    completer.ReplySuccess();
  } else {
    completer.ReplyError(status);
  }
}

std::optional<fidl::WireSyncClient<broker_fidl::LeaseControl>>
StateControlAdminServer::AcquireShutdownControlLease() const {
  if (!lessor_client_) {
    fprintf(
        stderr,
        "[shutdown-shim]: no lessor client available, skipping acquiring shutdown_control lease.\n");
    return std::nullopt;
  }

  auto resp = lessor_client_->Lease(static_cast<uint8_t>(ShutdownControlLevel::kActive));
  if (resp.status() != ZX_OK) {
    fprintf(stderr, "[shutdown-shim]: failed to request lease: %s\n", resp.status_string());

    // At this point, shutdown-shim has communicated with system-activity-governor and power-broker,
    // but the state of the system is inconsistent with that. Force reboot to recover.
    fprintf(stderr, "[shutdown-shim]: system is in an unexpected state, rebooting to recover\n");
    exit(1);
  }
  if (resp.value().is_error()) {
    fprintf(stderr, "[shutdown-shim]: failed to acquire lease: %d\n",
            static_cast<uint32_t>(resp.value().error_value()));

    // At this point, shutdown-shim has communicated with system-activity-governor and power-broker,
    // but the state of the system is inconsistent with that. Force reboot to recover.
    fprintf(stderr, "[shutdown-shim]: system is in an unexpected state, rebooting to recover\n");
    exit(1);
  }

  auto lease_control_client = fidl::WireSyncClient{std::move(resp.value().value()->lease_control)};

  auto status = broker_fidl::wire::LeaseStatus::kUnknown;
  while (status != broker_fidl::wire::LeaseStatus::kSatisfied) {
    auto status_resp = lease_control_client->WatchStatus(status);
    if (status_resp.status() != ZX_OK) {
      fprintf(stderr, "[shutdown-shim]: failed to watch lease: %s\n", resp.status_string());

      // At this point, shutdown-shim has communicated with system-activity-governor and
      // power-broker, but the state of the system is inconsistent with that.
      // Force reboot to recover.
      fprintf(stderr, "[shutdown-shim]: system is in an unexpected state, rebooting to recover\n");
      exit(1);
    }
    status = status_resp.value().status;
  }

  fprintf(stderr, "[shutdown-shim]: acquired shutdown_control lease\n");
  return std::optional(std::move(lease_control_client));
}

void SystemStateTransitionServer::GetTerminationSystemState(
    GetTerminationSystemStateCompleter::Sync& completer) {
  fbl::AutoLock al(&lock_);
  completer.Reply(system_power_state_);
}
void SystemStateTransitionServer::GetMexecZbis(GetMexecZbisCompleter::Sync& completer) {
  fbl::AutoLock al(&lock_);
  if (system_power_state_ != fuchsia_device_manager::SystemPowerState::kMexec) {
    return completer.ReplyError(ZX_ERR_BAD_STATE);
  }
  completer.ReplySuccess(std::move(mexec_kernel_zbi_), std::move(mexec_data_zbi_));
}

zx_status_t StateControlAdminServer::ExportServices(fbl::RefPtr<fs::PseudoDir>& svc_dir,
                                                    async_dispatcher* dispatcher) {
  zx_status_t status =
      svc_dir->AddEntry(fidl::DiscoverableProtocolName<statecontrol_fidl::Admin>,
                        fbl::MakeRefCounted<fs::Service>(
                            // `fuchsia.hardware.power.statecontrol.Admin| must run on a separate
                            // thread from `fuchsia.device.manager.SystemStateTransition` as the
                            // latter has to serve requests while the former makes blocking calls.
                            // Notice the different dispatcher specified here vs below.
                            [dispatcher = loop_.dispatcher(),
                             this](fidl::ServerEnd<statecontrol_fidl::Admin> server_end) mutable {
                              fidl::BindServer(dispatcher, std::move(server_end), this);
                              return ZX_OK;
                            }));
  if (status != ZX_OK) {
    return status;
  }
  return svc_dir->AddEntry(
      fidl::DiscoverableProtocolName<fuchsia_device_manager::SystemStateTransition>,
      fbl::MakeRefCounted<fs::Service>(
          // See above comment about how this must run on a separate thread from the other service.
          [dispatcher, this](
              fidl::ServerEnd<fuchsia_device_manager::SystemStateTransition> server_end) mutable {
            fidl::BindServer(dispatcher, std::move(server_end), &system_state_transition_server_);
            return ZX_OK;
          }));
}

int main() {
  zx_status_t status = StdoutToDebuglog::Init();
  if (status != ZX_OK) {
    return status;
  }
  printf("[shutdown-shim]: started\n");

  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);

  fs::ManagedVfs outgoing_vfs((loop.dispatcher()));
  auto outgoing_dir = fbl::MakeRefCounted<fs::PseudoDir>();
  auto svc_dir = fbl::MakeRefCounted<fs::PseudoDir>();
  StateControlAdminServer state_control_server;

  status = state_control_server.ExportServices(svc_dir, loop.dispatcher());
  if (status != ZX_OK) {
    fprintf(stderr, "[shutdown-shim]: ExportServices failed with %s\n",
            zx_status_get_string(status));
    return status;
  }
  outgoing_dir->AddEntry("svc", std::move(svc_dir));

  outgoing_vfs.ServeDirectory(
      outgoing_dir,
      fidl::ServerEnd<fio::Directory>{zx::channel(zx_take_startup_handle(PA_DIRECTORY_REQUEST))});

  loop.Run();

  fprintf(stderr, "[shutdown-shim]: exited unexpectedly\n");
  return 1;
}
