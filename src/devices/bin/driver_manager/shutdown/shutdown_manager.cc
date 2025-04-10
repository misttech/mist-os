// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "shutdown_manager.h"

#include <fidl/fuchsia.boot/cpp/wire.h>
#include <fidl/fuchsia.kernel/cpp/wire.h>
#include <lib/fidl/cpp/wire/channel.h>  // fidl::WireCall
#include <lib/zbi-format/zbi.h>
#include <lib/zbitl/error-string.h>
#include <lib/zbitl/image.h>
#include <lib/zbitl/item.h>
#include <lib/zbitl/vmo.h>
#include <zircon/processargs.h>  // PA_LIFECYCLE
#include <zircon/syscalls/system.h>

#include <src/bringup/lib/mexec/mexec.h>
#include <src/devices/lib/log/log.h>
#include <src/lib/fsl/vmo/sized_vmo.h>
#include <src/lib/fsl/vmo/vector.h>

namespace driver_manager {
namespace {

struct MexecVmos {
  zx::vmo kernel_zbi;
  zx::vmo data_zbi;
};

zx::result<MexecVmos> GetMexecZbis(zx::unowned_resource mexec_resource) {
  zx::result client = component::Connect<fuchsia_system_state::SystemStateTransition>();
  if (client.is_error()) {
    LOGF(ERROR, "Failed to connect to StateStateTransition: %s", client.status_string());
    return client.take_error();
  }

  fidl::Result result = fidl::Call(*client)->GetMexecZbis();
  if (result.is_error()) {
    LOGF(ERROR, "Failed to get mexec zbis: %s", result.error_value().FormatDescription().c_str());
    zx_status_t status = result.error_value().is_domain_error()
                             ? result.error_value().domain_error()
                             : result.error_value().framework_error().status();
    return zx::error(status);
  }
  zx::vmo& kernel_zbi = result->kernel_zbi();
  zx::vmo& data_zbi = result->data_zbi();

  if (zx_status_t status = mexec::PrepareDataZbi(std::move(mexec_resource), data_zbi.borrow());
      status != ZX_OK) {
    LOGF(ERROR, "Failed to prepare mexec data ZBI: %s", zx_status_get_string(status));
    return zx::error(status);
  }

  zx::result connect_result = component::Connect<fuchsia_boot::Items>();
  if (connect_result.is_error()) {
    LOGF(ERROR, "Failed to connect to fuchsia.boot::Items: %s", connect_result.status_string());
    return connect_result.take_error();
  }
  fidl::WireSyncClient<fuchsia_boot::Items> items(std::move(connect_result).value());

  // Driver metadata that the driver framework generally expects to be present.
  constexpr std::array kItemsToAppend{ZBI_TYPE_DRV_MAC_ADDRESS, ZBI_TYPE_DRV_PARTITION_MAP,
                                      ZBI_TYPE_DRV_BOARD_PRIVATE, ZBI_TYPE_DRV_BOARD_INFO};
  zbitl::Image data_image{data_zbi.borrow()};
  for (uint32_t type : kItemsToAppend) {
    std::string_view name = zbitl::TypeName(type);

    // TODO(https://fxbug.dev/42053781): Use a method that returns all matching items of
    // a given type instead of guessing possible `extra` values.
    for (uint32_t extra : std::array{0, 1, 2}) {
      fidl::WireResult result = items->Get(type, extra);
      if (!result.ok()) {
        return zx::error(result.status());
      }
      if (!result.value().payload.is_valid()) {
        // Absence is signified with an empty result value.
        LOGF(INFO, "No %.*s item (%#xu) present to append to mexec data ZBI",
             static_cast<int>(name.size()), name.data(), type);
        continue;
      }
      fsl::SizedVmo payload(std::move(result.value().payload), result.value().length);

      std::vector<char> contents;
      if (!fsl::VectorFromVmo(payload, &contents)) {
        LOGF(ERROR, "Failed to read contents of %.*s item (%#xu)", static_cast<int>(name.size()),
             name.data(), type);
        return zx::error(ZX_ERR_INTERNAL);
      }

      if (fit::result result = data_image.Append(zbi_header_t{.type = type, .extra = extra},
                                                 zbitl::AsBytes(contents));
          result.is_error()) {
        LOGF(ERROR, "Failed to append %.*s item (%#xu) to mexec data ZBI: %s",
             static_cast<int>(name.size()), name.data(), type,
             zbitl::ViewErrorString(result.error_value()).c_str());
        return zx::error(ZX_ERR_INTERNAL);
      }
    }
  }

  return zx::ok(MexecVmos{
      .kernel_zbi = std::move(kernel_zbi),
      .data_zbi = std::move(data_zbi),
  });
}

SystemPowerState GetSystemPowerState() {
  zx::result client = component::Connect<fuchsia_system_state::SystemStateTransition>();
  if (client.is_error()) {
    LOGF(ERROR, "Failed to connect to StateStateTransition: %s, falling back to default",
         client.status_string());
    return SystemPowerState::kReboot;
  }

  fidl::Result result = fidl::Call(*client)->GetTerminationSystemState();
  if (result.is_error()) {
    LOGF(ERROR, "Failed to get termination system state: %s, falling back to default",
         result.error_value().FormatDescription().c_str());
    return SystemPowerState::kReboot;
  }

  return result->state();
}

template <typename SyncCompleter>
fit::callback<void(zx_status_t)> ToCallback(SyncCompleter& completer) {
  return [completer = completer.ToAsync()](zx_status_t status) mutable { completer.Close(status); };
}

// Get the power resource from the root resource service. Not receiving the
// startup handle is logged, but not fatal.  In test environments, it would not
// be present.
zx::result<zx::resource> get_power_resource() {
  zx::result client_end = component::Connect<fuchsia_kernel::PowerResource>();
  if (client_end.is_error()) {
    return client_end.take_error();
  }
  fidl::WireResult result = fidl::WireCall(client_end.value())->Get();
  if (!result.ok()) {
    return zx::error(result.status());
  }
  return zx::ok(std::move(result.value().resource));
}

// Get the mexec resource from the mexec resource service. Not receiving the
// startup handle is logged, but not fatal.  In test environments, it would not
// be present.
zx::result<zx::resource> get_mexec_resource() {
  zx::result client_end = component::Connect<fuchsia_kernel::MexecResource>();
  if (client_end.is_error()) {
    return client_end.take_error();
  }
  fidl::WireResult result = fidl::WireCall(client_end.value())->Get();
  if (!result.ok()) {
    return zx::error(result.status());
  }
  return zx::ok(std::move(result.value().resource));
}

}  // anonymous namespace

ShutdownManager::ShutdownManager(NodeRemover* node_remover, async_dispatcher_t* dispatcher)
    : node_remover_(node_remover),
      devfs_lifecycle_(
          [this](fit::callback<void(zx_status_t)> cb) { SignalBootShutdown(std::move(cb)); }),
      devfs_with_pkg_lifecycle_(
          [this](fit::callback<void(zx_status_t)> cb) { SignalPackageShutdown(std::move(cb)); }),
      dispatcher_(dispatcher) {
  if (zx::result power_resource = get_power_resource(); power_resource.is_error()) {
    LOGF(INFO, "Failed to get root resource, assuming test environment and continuing (%s)",
         power_resource.status_string());
  } else {
    power_resource_ = std::move(power_resource.value());
  }
  if (zx::result mexec_resource = get_mexec_resource(); mexec_resource.is_error()) {
    LOGF(INFO, "Failed to get mexec resource, assuming test environment and continuing (%s)",
         mexec_resource.status_string());
  } else {
    mexec_resource_ = std::move(mexec_resource.value());
  }
}

// Invoked when the channel is closed or on any binding-related error.
// If we were not shutting down, we should start shutting down, because
// we no longer have a way to get signals to shutdown the system.
void ShutdownManager::OnUnbound(const char* connection, fidl::UnbindInfo info) {
  if (info.is_user_initiated()) {
    LOGF(DEBUG, "%s connection to ShutdownManager got unbound: %s", connection,
         info.FormatDescription().c_str());
  } else {
    LOGF(ERROR, "%s connection to ShutdownManager got unbound: %s", connection,
         info.FormatDescription().c_str());
  }
  SignalBootShutdown(nullptr);
}

void ShutdownManager::Publish(component::OutgoingDirectory& outgoing) {
  zx::result result = outgoing.AddUnmanagedProtocol<fuchsia_process_lifecycle::Lifecycle>(
      lifecycle_bindings_.CreateHandler(&devfs_lifecycle_, dispatcher_,
                                        fidl::kIgnoreBindingClosure),
      "fuchsia.device.fs.lifecycle.Lifecycle");
  ZX_ASSERT_MSG(result.is_ok(), "%s", result.status_string());

  result = outgoing.AddUnmanagedProtocol<fuchsia_process_lifecycle::Lifecycle>(
      lifecycle_bindings_.CreateHandler(&devfs_with_pkg_lifecycle_, dispatcher_,
                                        fidl::kIgnoreBindingClosure),
      "fuchsia.device.fs.with.pkg.lifecycle.Lifecycle");
  ZX_ASSERT_MSG(result.is_ok(), "%s", result.status_string());

  // Bind to lifecycle server
  fidl::ServerEnd<fuchsia_process_lifecycle::Lifecycle> lifecycle_server(
      zx::channel(zx_take_startup_handle(PA_LIFECYCLE)));

  if (lifecycle_server.is_valid()) {
    lifecycle_bindings_.AddBinding(dispatcher_, std::move(lifecycle_server), this,
                                   [](ShutdownManager* server, fidl::UnbindInfo info) {
                                     server->OnUnbound("Lifecycle", info);
                                   });
  } else {
    LOGF(INFO,
         "No valid handle found for lifecycle events, assuming test environment and continuing");
  }
}

void ShutdownManager::OnPackageShutdownComplete() {
  LOGF(INFO, "Package shutdown complete");
  ZX_ASSERT(shutdown_state_ == State::kPackageStopping);
  shutdown_state_ = State::kPackageStopped;
  for (auto& callback : package_shutdown_complete_callbacks_) {
    callback(ZX_OK);
  }
  package_shutdown_complete_callbacks_.clear();
  if (received_boot_shutdown_signal_) {
    shutdown_state_ = State::kBootStopping;
    // In the middle of package shutdown we were told to shutdown everything.
    node_remover_->ShutdownAllDrivers(
        fit::bind_member(this, &ShutdownManager::OnBootShutdownComplete));
  }
}

void ShutdownManager::OnBootShutdownComplete() {
  ZX_ASSERT(shutdown_state_ == State::kBootStopping);
  shutdown_state_ = State::kStopped;
  SystemExecute();
  for (auto& callback : boot_shutdown_complete_callbacks_) {
    callback(ZX_OK);
  }
  boot_shutdown_complete_callbacks_.clear();
}

void ShutdownManager::SignalPackageShutdown(fit::callback<void(zx_status_t)> cb) {
  // Switch our logs to go to debuglog to ensure they are flushed and available in the crashlog.
  driver_logger::GetLogger().SwitchToStdout();

  // Expected case: we get the call during kPackageStopping, or right before.
  // Store the completer for when we finish.
  if (shutdown_state_ == State::kRunning || shutdown_state_ == State::kPackageStopping) {
    package_shutdown_complete_callbacks_.emplace_back(std::move(cb));
    if (shutdown_state_ == State::kRunning) {
      shutdown_state_ = State::kPackageStopping;
      node_remover_->ShutdownPkgDrivers(
          fit::bind_member(this, &ShutdownManager::OnPackageShutdownComplete));
    }
  } else {
    // Otherwise, we already finished package shutdown or we have already jumped
    // to doing a full shutdown. Notify the callback.
    cb(ZX_OK);
  }
}

void ShutdownManager::Stop(StopCompleter::Sync& completer) {
  lifecycle_stop_ = true;
  SignalBootShutdown(ToCallback(completer));
}

void ShutdownManager::SignalBootShutdown(fit::callback<void(zx_status_t)> cb) {
  if (cb) {
    if (shutdown_state_ == State::kStopped) {
      cb(ZX_OK);
    } else {
      boot_shutdown_complete_callbacks_.emplace_back(std::move(cb));
    }
  }
  received_boot_shutdown_signal_ = true;
  // Expected case: we get the call while running, or after we shutdown the package drivers.
  if (shutdown_state_ == State::kRunning || shutdown_state_ == State::kPackageStopped) {
    shutdown_state_ = State::kBootStopping;
    node_remover_->ShutdownAllDrivers(
        fit::bind_member(this, &ShutdownManager::OnBootShutdownComplete));
  } else if (shutdown_state_ == State::kBootStopping) {
    LOGF(ERROR, "SignalBootShutdown() called during shutdown.");
  }
}

void ShutdownManager::SystemExecute() {
  auto shutdown_system_state = GetSystemPowerState();
  LOGF(INFO, "Suspend fallback with flags %#08hhx", shutdown_system_state);
  const char* what = "zx_system_powerctl";
  zx_status_t status = ZX_OK;
  if (!mexec_resource_.is_valid() || !power_resource_.is_valid()) {
    LOGF(WARNING, "Invalid Power/mexec resources. Assuming test.");
    if (lifecycle_stop_) {
      exit(0);
    }
    return;
  }
  switch (shutdown_system_state) {
    case SystemPowerState::kReboot:
      status = zx_system_powerctl(power_resource_.get(), ZX_SYSTEM_POWERCTL_REBOOT, nullptr);
      break;
    case SystemPowerState::kRebootBootloader:
      status =
          zx_system_powerctl(power_resource_.get(), ZX_SYSTEM_POWERCTL_REBOOT_BOOTLOADER, nullptr);
      break;
    case SystemPowerState::kRebootRecovery:
      status =
          zx_system_powerctl(power_resource_.get(), ZX_SYSTEM_POWERCTL_REBOOT_RECOVERY, nullptr);
      break;
    case SystemPowerState::kRebootKernelInitiated:
      status = zx_system_powerctl(power_resource_.get(),
                                  ZX_SYSTEM_POWERCTL_ACK_KERNEL_INITIATED_REBOOT, nullptr);
      if (status == ZX_OK) {
        // Sleep indefinitely to give the kernel a chance to reboot the system. This results in a
        // cleaner reboot because it prevents driver_manager from exiting. If driver_manager exits
        // the other parts of the system exit, bringing down the root job. Crashing the root job
        // is innocuous at this point, but we try to avoid it to reduce log noise and possible
        // confusion.
        while (true) {
          sleep(5 * 60);
          // We really shouldn't still be running, so log if we are. Use `printf`
          // because messages from the devices are probably only visible over
          // serial at this point.
          printf("driver_manager: unexpectedly still running after successful reboot syscall\n");
        }
      }
      break;
    case SystemPowerState::kPoweroff:
      status = zx_system_powerctl(power_resource_.get(), ZX_SYSTEM_POWERCTL_SHUTDOWN, nullptr);
      break;
    case SystemPowerState::kMexec: {
      LOGF(INFO, "About to mexec...");
      zx::result<MexecVmos> mexec_vmos = GetMexecZbis(mexec_resource_.borrow());
      status = mexec_vmos.status_value();
      if (status == ZX_OK) {
        status = mexec::BootZbi(mexec_resource_.borrow(), std::move(mexec_vmos->kernel_zbi),
                                std::move(mexec_vmos->data_zbi));
      }
      what = "zx_system_mexec";
      break;
    }
    case SystemPowerState::kFullyOn:
    case SystemPowerState::kSuspendRam:
      LOGF(ERROR, "Unexpected shutdown state requested: %hhu", shutdown_system_state);
      break;
  }

  // This is mainly for test dev:
  if (lifecycle_stop_) {
    LOGF(INFO, "Exiting driver manager gracefully");
    // TODO(fxb:52627) This event handler should teardown devices and driver hosts
    // properly for system state transitions where driver manager needs to go down.
    // Exiting like so, will not run all the destructors and clean things up properly.
    // Instead the main devcoordinator loop should be quit.
    exit(0);
  }

  // Warning - and not an error - as a large number of tests unfortunately rely
  // on this syscall actually failing.
  LOGF(WARNING, "%s: %s", what, zx_status_get_string(status));
}

}  // namespace driver_manager
