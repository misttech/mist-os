// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "suspend_handler.h"

#include <fidl/fuchsia.device.manager/cpp/fidl.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/fdio/directory.h>
#include <lib/zbitl/error-string.h>
#include <lib/zbitl/image.h>
#include <lib/zbitl/item.h>
#include <lib/zbitl/vmo.h>
#include <zircon/syscalls/system.h>

#include <inspector/inspector.h>

#include "src/bringup/lib/mexec/mexec.h"
#include "src/devices/bin/driver_manager/v1/coordinator.h"
#include "src/devices/bin/driver_manager/v1/driver_host.h"
#include "src/devices/lib/log/log.h"
#include "src/lib/fsl/vmo/vector.h"

namespace {

struct MexecVmos {
  zx::vmo kernel_zbi;
  zx::vmo data_zbi;
};

zx::result<MexecVmos> GetMexecZbis(zx::unowned_resource mexec_resource) {
  zx::result client = component::Connect<fuchsia_device_manager::SystemStateTransition>();
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
  fidl::WireSyncClient items(std::move(connect_result.value()));

  // Driver metadata that the driver framework generally expects to be present.
  constexpr std::array kItemsToAppend{ZBI_TYPE_DRV_MAC_ADDRESS, ZBI_TYPE_DRV_PARTITION_MAP,
                                      ZBI_TYPE_DRV_BOARD_PRIVATE, ZBI_TYPE_DRV_BOARD_INFO};
  zbitl::Image data_image{data_zbi.borrow()};
  for (uint32_t type : kItemsToAppend) {
    std::string_view name = zbitl::TypeName(type);

    // TODO(fxbug.dev/102804): Use a method that returns all matching items of
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

void SuspendFallback(const zx::resource& root_resource, const zx::resource& mexec_resource,
                     uint32_t flags) {
  LOGF(INFO, "Suspend fallback with flags %#08x", flags);
  const char* what = "zx_system_powerctl";
  zx_status_t status = ZX_OK;
  if (flags == DEVICE_SUSPEND_FLAG_REBOOT) {
    status = zx_system_powerctl(root_resource.get(), ZX_SYSTEM_POWERCTL_REBOOT, nullptr);
  } else if (flags == DEVICE_SUSPEND_FLAG_REBOOT_BOOTLOADER) {
    status = zx_system_powerctl(root_resource.get(), ZX_SYSTEM_POWERCTL_REBOOT_BOOTLOADER, nullptr);
  } else if (flags == DEVICE_SUSPEND_FLAG_REBOOT_RECOVERY) {
    status = zx_system_powerctl(root_resource.get(), ZX_SYSTEM_POWERCTL_REBOOT_RECOVERY, nullptr);
  } else if (flags == DEVICE_SUSPEND_FLAG_REBOOT_KERNEL_INITIATED) {
    status = zx_system_powerctl(root_resource.get(), ZX_SYSTEM_POWERCTL_ACK_KERNEL_INITIATED_REBOOT,
                                nullptr);
    if (status == ZX_OK) {
      // Sleep indefinitely to give the kernel a chance to reboot the system. This results in a
      // cleaner reboot because it prevents driver_manager from exiting. If driver_manager exits the
      // other parts of the system exit, bringing down the root job. Crashing the root job is
      // innocuous at this point, but we try to avoid it to reduce log noise and possible confusion.
      while (true) {
        sleep(5 * 60);
        // We really shouldn't still be running, so log if we are. Use `printf`
        // because messages from the devices are probably only visible over
        // serial at this point.
        printf("driver_manager: unexpectedly still running after successful reboot syscall\n");
      }
    }
  } else if (flags == DEVICE_SUSPEND_FLAG_POWEROFF) {
    status = zx_system_powerctl(root_resource.get(), ZX_SYSTEM_POWERCTL_SHUTDOWN, nullptr);
  } else if (flags == DEVICE_SUSPEND_FLAG_MEXEC) {
    LOGF(INFO, "About to mexec...");
    zx::result<MexecVmos> mexec_vmos = GetMexecZbis(mexec_resource.borrow());
    status = mexec_vmos.status_value();
    if (status == ZX_OK) {
      status = mexec::BootZbi(mexec_resource.borrow(), std::move(mexec_vmos->kernel_zbi),
                              std::move(mexec_vmos->data_zbi));
    }
    what = "zx_system_mexec";
  }
  // Warning - and not an error - as a large number of tests unfortunately rely
  // on this syscall actually failing.
  LOGF(WARNING, "%s: %s", what, zx_status_get_string(status));
}

void DumpSuspendTaskDependencies(const SuspendTask* task, int depth = 0) {
  ZX_ASSERT(task != nullptr);

  const char* task_status = "";
  if (task->is_completed()) {
    task_status = zx_status_get_string(task->status());
  } else {
    bool dependence = false;
    for (const auto* dependency : task->Dependencies()) {
      if (!dependency->is_completed()) {
        dependence = true;
        break;
      }
    }
    task_status = dependence ? "<dependence>" : "Stuck <suspending>";
    if (!dependence) {
      zx_koid_t pid = task->device().host()->koid();
      if (!pid) {
        return;
      }
      zx::unowned_process process = task->device().host()->proc();
      char process_name[ZX_MAX_NAME_LEN];
      zx_status_t status = process->get_property(ZX_PROP_NAME, process_name, sizeof(process_name));
      if (status != ZX_OK) {
        strlcpy(process_name, "unknown", sizeof(process_name));
      }
      printf("Backtrace of threads of process %lu:%s\n", pid, process_name);
      inspector_print_debug_info_for_all_threads(stdout, process->get());
      fflush(stdout);
    }
  }
  LOGF(INFO, "%*cSuspend %s: %s", 2 * depth, ' ', task->device().name().data(), task_status);
  for (const auto* dependency : task->Dependencies()) {
    DumpSuspendTaskDependencies(reinterpret_cast<const SuspendTask*>(dependency), depth + 1);
  }
}

}  // namespace

SuspendHandler::SuspendHandler(Coordinator* coordinator, zx::duration suspend_timeout)
    : coordinator_(coordinator), suspend_timeout_(suspend_timeout) {}

void SuspendHandler::Suspend(uint32_t flags, SuspendCallback callback) {
  // The root device should have a proxy. If not, the system hasn't fully initialized yet and
  // cannot go to suspend.
  if (!coordinator_->root_device() || !coordinator_->root_device()->proxy()) {
    LOGF(ERROR, "Aborting system-suspend, system is not fully initialized yet");
    if (callback) {
      callback(ZX_ERR_UNAVAILABLE);
    }
    return;
  }

  // We shouldn't have two tasks in progress at the same time.
  if (AnyTasksInProgress()) {
    LOGF(ERROR, "Aborting system-suspend, there's a task in progress.");
    callback(ZX_ERR_UNAVAILABLE);
  }

  // The system is already suspended.
  if (flags_ == Flags::kSuspend) {
    LOGF(ERROR, "Aborting system-suspend, the system is already suspended");
    if (callback) {
      callback(ZX_ERR_ALREADY_EXISTS);
    }
    return;
  }

  flags_ = Flags::kSuspend;
  sflags_ = flags;
  suspend_callback_ = std::move(callback);

  LOGF(INFO, "Creating a suspend timeout-watchdog\n");
  auto watchdog_task = std::make_unique<async::TaskClosure>([this] {
    if (!InSuspend()) {
      return;  // Suspend failed to complete.
    }
    LOGF(ERROR, "Device suspend timed out, suspend flags: %#08x", sflags_);
    if (suspend_task_.get() != nullptr) {
      DumpSuspendTaskDependencies(suspend_task_.get());
    }

    SuspendFallback(coordinator_->root_resource(), coordinator_->mexec_resource(), sflags_);
    // Unless in test env, we should not reach here.
    if (suspend_callback_) {
      suspend_callback_(ZX_ERR_TIMED_OUT);
    }
  });
  suspend_watchdog_task_ = std::move(watchdog_task);
  zx_status_t status =
      suspend_watchdog_task_->PostDelayed(coordinator_->dispatcher(), suspend_timeout_);
  if (status != ZX_OK) {
    LOGF(ERROR, "Failed to create timeout watchdog for suspend: %s\n",
         zx_status_get_string(status));
  }
  auto completion = [this](zx_status_t status) {
    suspend_watchdog_task_->Cancel();
    if (status != ZX_OK) {
      // TODO: unroll suspend
      // do not continue to suspend as this indicates a driver suspend
      // problem and should show as a bug
      // TODO(https://fxbug.dev/56208): Change this log back to error once isolated devmgr is fixed.
      LOGF(WARNING, "Failed to suspend: %s", zx_status_get_string(status));
      flags_ = SuspendHandler::Flags::kRunning;
      if (suspend_callback_) {
        suspend_callback_(status);
      }
      return;
    }

    // Although this is called the SuspendFallback we expect to end up here for most operations
    // that execute a flavor of reboot because Zircon can handle most reboot operations on most
    // platforms.
    SuspendFallback(coordinator_->root_resource(), coordinator_->mexec_resource(), sflags_);
    // if we get here the system did not suspend successfully
    flags_ = SuspendHandler::Flags::kRunning;

    if (suspend_callback_) {
      suspend_callback_(ZX_OK);
    }
  };

  suspend_task_ = SuspendTask::Create(coordinator_->root_device(), sflags_, std::move(completion));
  LOGF(INFO, "Successfully created suspend task on device 'sys'");
}

void SuspendHandler::UnregisterSystemStorageForShutdown(SuspendCallback callback) {
  // We shouldn't have two tasks in progress at the same time.
  if (AnyTasksInProgress()) {
    LOGF(ERROR, "Aborting UnregisterSystemStorageForShutdown, there's a task in progress.");
    callback(ZX_ERR_UNAVAILABLE);
  }

  // Only set flags_ if we are going from kRunning -> kStorageSuspend. It's possible that
  // flags are kSuspend here but Suspend() is calling us first to clean up the filesystem drivers.
  if (flags_ == Flags::kRunning) {
    flags_ = Flags::kStorageSuspend;
  }

  SuspendMatchingTask::Match match = [](const Device& device) {
    return device.DriverLivesInSystemStorage();
  };

  uint32_t sflags = coordinator_->suspend_resume_manager().GetSuspendFlagsFromSystemPowerState(
      coordinator_->shutdown_system_state());

  unregister_system_storage_task_ = SuspendMatchingTask::Create(
      coordinator_->root_device(), sflags, std::move(match),
      [this, callback = std::move(callback)](zx_status_t status) mutable {
        unregister_system_storage_task_ = nullptr;
        callback(status);
      });
}

bool SuspendHandler::AnyTasksInProgress() {
  if (suspend_task_.get() != nullptr && !suspend_task_->is_completed()) {
    return true;
  }
  if (unregister_system_storage_task_.get() != nullptr &&
      !unregister_system_storage_task_->is_completed()) {
    return true;
  }
  return false;
}
