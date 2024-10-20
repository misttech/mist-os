// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.power.statecontrol/cpp/wire.h>
#include <fidl/fuchsia.power.system/cpp/wire.h>
#include <fidl/fuchsia.sys2/cpp/wire.h>
#include <fidl/fuchsia.system.state/cpp/wire.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/zx/eventpair.h>
#include <zircon/errors.h>
#include <zircon/process.h>
#include <zircon/processargs.h>
#include <zircon/status.h>

#include <chrono>
#include <cstdio>
#include <memory>
#include <optional>
#include <thread>

#include <fbl/auto_lock.h>
#include <fbl/mutex.h>

#include "lib/async/default.h"
#include "src/storage/lib/vfs/cpp/managed_vfs.h"
#include "src/storage/lib/vfs/cpp/pseudo_dir.h"
#include "src/storage/lib/vfs/cpp/service.h"
#include "src/sys/lib/stdout-to-debuglog/cpp/stdout-to-debuglog.h"

namespace fio = fuchsia_io;

namespace statecontrol_fidl = fuchsia_hardware_power_statecontrol;
namespace sys2_fidl = fuchsia_sys2;
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

template <typename Protocol>
zx::result<fidl::ClientEnd<Protocol>> connect_to_protocol_with_timeout(
    const char* name = fidl::DiscoverableProtocolDefaultPath<Protocol>);

class RebootWatcher;

class SystemStateTransitionServer final
    : public fidl::WireServer<fuchsia_system_state::SystemStateTransition> {
 public:
  SystemStateTransitionServer() {
    // TODO(https://fxbug.dev/368440548) Fix deeper problems and call Prepare directly.
    async::PostDelayedTask(async_get_default_dispatcher(), [this]() { Prepare(); }, zx::sec(15));
  }

  // fuchsia.system.state/SystemStateTransition APIs.
  void GetTerminationSystemState(GetTerminationSystemStateCompleter::Sync& completer) override;
  void GetMexecZbis(GetMexecZbisCompleter::Sync& completer) override;

  void set_system_power_state(fuchsia_system_state::SystemPowerState system_power_state) {
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
  void Prepare() {
    zx::result local = component::Connect<statecontrol_fidl::RebootMethodsWatcherRegister>(
        fidl::DiscoverableProtocolDefaultPath<statecontrol_fidl::RebootMethodsWatcherRegister>);
    if (local.is_ok()) {
      zx::result watcher_channels =
          fidl::CreateEndpoints<statecontrol_fidl::RebootMethodsWatcher>();
      watcher_ = std::make_unique<RebootWatcher>(std::move(watcher_channels->server), this,
                                                 async_get_default_dispatcher());
      fidl::WireResult res =
          fidl::WireCall(local.value())->RegisterWithAck(std::move(watcher_channels->client));
      if (!res.ok()) {
        fprintf(stderr, "[shutdown-shim]: RegisterWithAck failed\n");
        return;
      }
      fprintf(stderr, "[shutdown-shim]: RegisterWithAck succeeded\n");
    } else {
      // It's fine this is not available in bootstrap
      fprintf(stderr, "[shutdown-shim]: Not able to connect to RebootMethodWatcherRegister\n");
    }
  }

  fbl::Mutex lock_;
  fuchsia_system_state::SystemPowerState system_power_state_ __TA_GUARDED(&lock_) =
      fuchsia_system_state::SystemPowerState::kFullyOn;
  zx::vmo mexec_kernel_zbi_ __TA_GUARDED(&lock_);
  zx::vmo mexec_data_zbi_ __TA_GUARDED(&lock_);
  std::unique_ptr<RebootWatcher> watcher_;
};

class RebootWatcher : public fidl::WireServer<statecontrol_fidl::RebootMethodsWatcher> {
 public:
  RebootWatcher(fidl::ServerEnd<statecontrol_fidl::RebootMethodsWatcher> server_end,
                SystemStateTransitionServer* system_state_transition_server,
                async_dispatcher_t* dispatcher)
      : system_state_transition_server_(system_state_transition_server) {
    fidl::BindServer(dispatcher, std::move(server_end), this);
  }
  RebootWatcher(const RebootWatcher&) = delete;
  RebootWatcher& operator=(const RebootWatcher&) = delete;
  RebootWatcher(RebootWatcher&&) = delete;
  RebootWatcher& operator=(RebootWatcher&&) = delete;

  void OnReboot(OnRebootRequestView request, OnRebootCompleter::Sync& completer) override {
    // Ignore other reasons because they are from shutdown-shim and are processed already.
    if (request->reason == statecontrol_fidl::RebootReason::kHighTemperature) {
      fuchsia_system_state::SystemPowerState target_state =
          fuchsia_system_state::SystemPowerState::kReboot;
      system_state_transition_server_->set_system_power_state(target_state);
    }
    completer.Reply();
  }

 private:
  SystemStateTransitionServer* system_state_transition_server_;
};

class StateControlAdminServer final : public fidl::WireServer<statecontrol_fidl::Admin> {
 public:
  StateControlAdminServer() : loop_((&kAsyncLoopConfigNoAttachToCurrentThread)) {
    loop_.StartThread("SystemStateTransitionLoop");
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
  std::optional<zx::eventpair> AcquireShutdownControlLease() const;

  SystemStateTransitionServer system_state_transition_server_;
  async::Loop loop_;
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
zx::result<fidl::ClientEnd<Protocol>> connect_to_protocol_with_timeout(const char* name) {
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
void drive_shutdown_manually(fuchsia_system_state::SystemPowerState state) {
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
                         fuchsia_system_state::SystemPowerState fallback_state,
                         const statecontrol_fidl::wire::RebootReason* reboot_reason = nullptr,
                         StateControlAdminServer::MexecRequestView* mexec_request = nullptr) {
  switch (fallback_state) {
    case fuchsia_system_state::SystemPowerState::kReboot: {
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
    case fuchsia_system_state::SystemPowerState::kRebootKernelInitiated: {
      auto resp = statecontrol_client->Reboot(*reboot_reason);
      if (resp.status() != ZX_OK) {
        return ZX_ERR_UNAVAILABLE;
      }
      if (resp.value().is_error()) {
        return resp.value().error_value();
      }
      return ZX_OK;

    } break;
    case fuchsia_system_state::SystemPowerState::kRebootBootloader: {
      auto resp = statecontrol_client->RebootToBootloader();
      if (resp.status() != ZX_OK) {
        return ZX_ERR_UNAVAILABLE;
      }
      if (resp->is_error()) {
        return resp->error_value();
      }
      return ZX_OK;

    } break;
    case fuchsia_system_state::SystemPowerState::kRebootRecovery: {
      auto resp = statecontrol_client->RebootToRecovery();
      if (resp.status() != ZX_OK) {
        return ZX_ERR_UNAVAILABLE;
      }
      if (resp->is_error()) {
        return resp->error_value();
      }
      return ZX_OK;

    } break;
    case fuchsia_system_state::SystemPowerState::kPoweroff: {
      auto resp = statecontrol_client->Poweroff();
      if (resp.status() != ZX_OK) {
        return ZX_ERR_UNAVAILABLE;
      }
      if (resp->is_error()) {
        return resp->error_value();
      }
      return ZX_OK;

    } break;
    case fuchsia_system_state::SystemPowerState::kMexec: {
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
    case fuchsia_system_state::SystemPowerState::kSuspendRam: {
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
zx_status_t forward_command(fuchsia_system_state::SystemPowerState fallback_state,
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
    if (fallback_state == fuchsia_system_state::SystemPowerState::kSuspendRam) {
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
  [[maybe_unused]] auto reboot_control_lease = AcquireShutdownControlLease();
  fuchsia_system_state::SystemPowerState target_state =
      fuchsia_system_state::SystemPowerState::kReboot;
  if (request->reason == statecontrol_fidl::wire::RebootReason::kOutOfMemory) {
    target_state = fuchsia_system_state::SystemPowerState::kRebootKernelInitiated;
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
  [[maybe_unused]] auto reboot_control_lease = AcquireShutdownControlLease();
  system_state_transition_server_.set_system_power_state(
      fuchsia_system_state::SystemPowerState::kRebootBootloader);
  zx_status_t status = forward_command(fuchsia_system_state::SystemPowerState::kRebootBootloader);
  if (status == ZX_OK) {
    completer.ReplySuccess();
  } else {
    completer.ReplyError(status);
  }
}

void StateControlAdminServer::RebootToRecovery(RebootToRecoveryCompleter::Sync& completer) {
  [[maybe_unused]] auto reboot_control_lease = AcquireShutdownControlLease();
  system_state_transition_server_.set_system_power_state(
      fuchsia_system_state::SystemPowerState::kRebootRecovery);
  zx_status_t status = forward_command(fuchsia_system_state::SystemPowerState::kRebootRecovery);
  if (status == ZX_OK) {
    completer.ReplySuccess();
  } else {
    completer.ReplyError(status);
  }
}

void StateControlAdminServer::Poweroff(PoweroffCompleter::Sync& completer) {
  [[maybe_unused]] auto reboot_control_lease = AcquireShutdownControlLease();
  system_state_transition_server_.set_system_power_state(
      fuchsia_system_state::SystemPowerState::kPoweroff);
  zx_status_t status = forward_command(fuchsia_system_state::SystemPowerState::kPoweroff);
  if (status == ZX_OK) {
    completer.ReplySuccess();
  } else {
    completer.ReplyError(status);
  }
}

void StateControlAdminServer::Mexec(MexecRequestView request, MexecCompleter::Sync& completer) {
  [[maybe_unused]] auto reboot_control_lease = AcquireShutdownControlLease();
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
      fuchsia_system_state::SystemPowerState::kMexec);
  system_state_transition_server_.set_mexec_kernel_zbi(std::move(kernel_zbi));
  system_state_transition_server_.set_mexec_data_zbi(std::move(data_zbi));

  zx::result local = connect_to_protocol_with_timeout<statecontrol_fidl::Admin>();
  if (local.is_ok()) {
    fprintf(stderr, "[shutdown-shim]: trying to forward Mexec command\n");
    zx_status_t status =
        send_command(fidl::WireSyncClient(std::move(local.value())),
                     fuchsia_system_state::SystemPowerState::kMexec, nullptr, &request);
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

  drive_shutdown_manually(fuchsia_system_state::SystemPowerState::kMexec);

  // We should block on fuchsia.sys.SystemController forever on this thread, if
  // it returns something has gone wrong.
  fprintf(stderr, "[shutdown-shim]: we shouldn't still be running, crashing the system\n");
  exit(1);
}

void StateControlAdminServer::SuspendToRam(SuspendToRamCompleter::Sync& completer) {
  system_state_transition_server_.set_system_power_state(
      fuchsia_system_state::SystemPowerState::kSuspendRam);
  zx_status_t status = forward_command(fuchsia_system_state::SystemPowerState::kSuspendRam);
  if (status == ZX_OK) {
    completer.ReplySuccess();
  } else {
    completer.ReplyError(status);
  }
}

std::optional<zx::eventpair> StateControlAdminServer::AcquireShutdownControlLease() const {
  zx::result activity_governor = component::Connect<system_fidl::ActivityGovernor>();
  if (activity_governor.is_error()) {
    fprintf(stderr, "[shutdown-shim]: error connecting to system_activity_governor: %s\n",
            activity_governor.status_string());
    return std::nullopt;
  }

  fidl::WireSyncClient<system_fidl::ActivityGovernor> sag_client =
      fidl::WireSyncClient{std::move(activity_governor.value())};

  auto resp = sag_client->TakeWakeLease("shutdown_control");
  if (resp.status() != ZX_OK) {
    fprintf(stderr, "[shutdown-shim]: failed to request lease: %s\n", resp.status_string());
    return std::nullopt;
  }

  return std::optional(std::move(resp.value().token));
}

void SystemStateTransitionServer::GetTerminationSystemState(
    GetTerminationSystemStateCompleter::Sync& completer) {
  fbl::AutoLock al(&lock_);
  completer.Reply(system_power_state_);
}
void SystemStateTransitionServer::GetMexecZbis(GetMexecZbisCompleter::Sync& completer) {
  fbl::AutoLock al(&lock_);
  if (system_power_state_ != fuchsia_system_state::SystemPowerState::kMexec) {
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
                            // thread from `fuchsia.system.state.SystemStateTransition` as the
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
      fidl::DiscoverableProtocolName<fuchsia_system_state::SystemStateTransition>,
      fbl::MakeRefCounted<fs::Service>(
          // See above comment about how this must run on a separate thread from the other service.
          [dispatcher,
           this](fidl::ServerEnd<fuchsia_system_state::SystemStateTransition> server_end) mutable {
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
