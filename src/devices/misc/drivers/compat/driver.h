// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_MISC_DRIVERS_COMPAT_DRIVER_H_
#define SRC_DEVICES_MISC_DRIVERS_COMPAT_DRIVER_H_

#include <fidl/fuchsia.boot/cpp/wire.h>
#include <fidl/fuchsia.scheduler/cpp/markers.h>
#include <lib/async/cpp/executor.h>
#include <lib/driver/compat/cpp/compat.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/devfs/cpp/connector.h>
#include <lib/fdf/cpp/dispatcher.h>
#include <lib/fpromise/scope.h>
#include <lib/inspect/component/cpp/component.h>

#include <unordered_set>

#include "src/devices/misc/drivers/compat/device.h"
#include "src/lib/storage/vfs/cpp/pseudo_dir.h"
#include "src/lib/storage/vfs/cpp/synchronous_vfs.h"

extern std::mutex kDriverGlobalsLock;
extern zx::resource kRootResource __TA_GUARDED(kDriverGlobalsLock);

namespace compat {

// Driver is the compatibility driver that loads DFv1 drivers.
class Driver : public fdf::DriverBase {
  using Base = fdf::DriverBase;

 public:
  Driver(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher driver_dispatcher,
         device_t device, const zx_protocol_device_t* ops, std::string_view driver_path);
  ~Driver() override;

  void Start(fdf::StartCompleter completer) override;
  void PrepareStop(fdf::PrepareStopCompleter completer) override;

  // Returns the context that DFv1 driver provided.
  void* Context() const;

  zx::result<zx::vmo> LoadFirmware(Device* device, const char* filename, size_t* size);

  // # Threading notes
  //
  // If this method is not called from a task running on |dispatcher|,
  // this method will schedule its work on that dispatcher then block until it
  // is done.
  zx_status_t AddDevice(Device* parent, device_add_args_t* args, zx_device_t** out);
  zx::result<> SetProfileByRole(zx::unowned_thread thread, std::string_view role);
  zx::result<std::string> GetVariable(const char* name);

  zx_status_t GetFragmentProtocol(const char* fragment, uint32_t proto_id, void* out);

  Device& GetDevice() { return device_; }

  void CompleteStart(zx::result<> result) { start_completer_.value()(result); }

  // These accessors are used by other classes in the compat driver so we want to expose
  // them publicly since they are protected in DriverBase.
  async_dispatcher_t* dispatcher() { return Base::dispatcher(); }
  const async_dispatcher_t* dispatcher() const { return Base::dispatcher(); }
  const fdf::Namespace& driver_namespace() { return *incoming(); }
  fdf::OutgoingDirectory& outgoing() { return *Base::outgoing(); }

  uint32_t GetNextDeviceId() { return next_device_id_++; }

  fs::PseudoDir& diagnostics_dir() { return *diagnostics_dir_; }

  const std::string& driver_path() const { return driver_path_; }

 private:
  bool IsComposite();

  bool IsRunningOnDispatcher() const;

  // If the current thread is running a task from our dispatcher, calls |task|
  // synchronously. Otherwise, schedules |task| onto our dispatcher and blocks
  // to receive its result.
  zx_status_t RunOnDispatcher(fit::callback<zx_status_t()> task);

  // Loads the driver using the provided `vmos`.
  zx::result<> LoadDriver(zx::vmo loader_vmo, zx::vmo driver_vmo);
  // Starts the DFv1 driver.
  zx::result<> StartDriver();

  // Attempts to trigger run_unit_tests driver hook if they are enabled.
  zx::result<> TryRunUnitTests();

  fpromise::promise<void, zx_status_t> ConnectToParentDevices();
  fpromise::promise<void, zx_status_t> GetDeviceInfo();

  // Serves the diagnostics directory that is used to host inspect files.
  zx_status_t ServeDiagnosticsDir();

  bool ShouldCallRelease() {
    // We purposefully leak in shutdown/reboot flows to emulate DFv1 shutdown. The fdf::Node client
    // should have been torn down by the driver runtime canceling all outstanding waits by the time
    // stop has been called, allowing shutdown to proceed.
    return record_ != nullptr && record_->ops->release != nullptr &&
           system_state_ == fuchsia_device_manager::SystemPowerState::kFullyOn;
  }

  async::Executor executor_;

  std::shared_ptr<fdf::Logger> inner_logger_;

  std::string driver_path_;
  std::string driver_name_;
  Device device_;

  fuchsia_device_manager::wire::SystemPowerState system_state_ =
      fuchsia_device_manager::wire::SystemPowerState::kFullyOn;

  std::unique_ptr<fs::SynchronousVfs> diagnostics_vfs_;
  fbl::RefPtr<fs::PseudoDir> diagnostics_dir_ = fbl::MakeRefCounted<fs::PseudoDir>();

  // The next unique device id for devices. Starts at 1 because `device_` has id zero.
  uint32_t next_device_id_ = 1;

  void* library_ = nullptr;
  zx_driver_rec_t* record_ = nullptr;
  void* context_ = nullptr;

  std::optional<fdf::StartCompleter> start_completer_;

  // API resources.
  zx::resource root_resource_;

  fidl::WireClient<fuchsia_driver_compat::Device> parent_client_;
  std::unordered_map<std::string, fidl::WireClient<fuchsia_driver_compat::Device>> parent_clients_;

  // NOTE: Must be the last member.
  fpromise::scope scope_;
};

class DriverFactory {
 public:
  static void CreateDriver(fdf::DriverStartArgs start_args,
                           fdf::UnownedSynchronizedDispatcher driver_dispatcher,
                           fdf::StartCompleter completer);
};

// |GlobalLoggerList| is global for the entire driver host process. It maintains a
// |LoggerInstances| for each driver_path that is active.
class GlobalLoggerList {
  friend ::zx_driver;
  // |LoggerInstances| is shared for all drivers with the same driver_path. The |loggers_| set
  // contains all the active instances.
  class LoggerInstances {
   public:
    explicit LoggerInstances(bool log_node_names) : log_node_names_(log_node_names) {}
    // Logs a message for the DFv1 driver.
    void Log(FuchsiaLogSeverity severity, const char* tag, const char* file, int line,
             const char* msg, va_list args);
    zx_driver_t* ZxDriver();
    void AddLogger(std::shared_ptr<fdf::Logger>& logger,
                   const std::optional<std::string>& node_name);
    void RemoveLogger(std::shared_ptr<fdf::Logger>& logger,
                      const std::optional<std::string>& node_name);

    size_t count() { return loggers_.size(); }
    const std::vector<std::string>& node_names_for_testing() { return node_names_; }

   private:
    bool log_node_names_;
    std::unordered_set<std::shared_ptr<fdf::Logger>> loggers_;
    std::vector<std::string> node_names_;
  };

 public:
  explicit GlobalLoggerList(bool log_node_names) : log_node_names_(log_node_names) {}
  zx_driver_t* AddLogger(const std::string& driver_path, std::shared_ptr<fdf::Logger>& logger,
                         const std::optional<std::string>& node_name);
  void RemoveLogger(const std::string& driver_path, std::shared_ptr<fdf::Logger>& logger,
                    const std::optional<std::string>& node_name);
  std::optional<size_t> loggers_count_for_testing(const std::string& driver_path);

 private:
  bool log_node_names_;
  std::unordered_map<std::string, LoggerInstances> instances_;
};

}  // namespace compat

struct zx_driver : public compat::GlobalLoggerList::LoggerInstances {
  // NOTE: Intentionally empty, do not add to this.
};

#endif  // SRC_DEVICES_MISC_DRIVERS_COMPAT_DRIVER_H_
