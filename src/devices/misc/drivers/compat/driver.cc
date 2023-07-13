// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/misc/drivers/compat/driver.h"

#include <fidl/fuchsia.device.manager/cpp/wire.h>
#include <fidl/fuchsia.scheduler/cpp/wire.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/ddk/binding_priv.h>
#include <lib/driver/compat/cpp/connect.h>
#include <lib/driver/component/cpp/internal/lifecycle.h>
#include <lib/driver/component/cpp/internal/start_args.h>
#include <lib/driver/component/cpp/internal/symbols.h>
#include <lib/driver/logging/cpp/structured_logger.h>
#include <lib/driver/promise/cpp/promise.h>
#include <lib/fidl/cpp/wire/connect_service.h>
#include <lib/fit/defer.h>
#include <lib/fpromise/bridge.h>
#include <lib/fpromise/promise.h>
#include <lib/fpromise/single_threaded_executor.h>
#include <lib/sync/cpp/completion.h>
#include <zircon/dlfcn.h>

#include "src/devices/lib/log/log.h"
#include "src/devices/misc/drivers/compat/loader.h"

namespace fboot = fuchsia_boot;
namespace fdf {
using namespace fuchsia_driver_framework;
}
namespace fio = fuchsia_io;
namespace fldsvc = fuchsia_ldsvc;
namespace fdm = fuchsia_device_manager;

using fpromise::bridge;
using fpromise::error;
using fpromise::join_promises;
using fpromise::ok;
using fpromise::promise;
using fpromise::result;

// This lock protects any globals, as globals could be accessed by other
// drivers and other threads within the process.
// Currently this protects the root resource and the loader service.
std::mutex kDriverGlobalsLock;

namespace {

constexpr auto kOpenFlags = fio::wire::OpenFlags::kRightReadable |
                            fio::wire::OpenFlags::kRightExecutable |
                            fio::wire::OpenFlags::kNotDirectory;
constexpr auto kVmoFlags = fio::wire::VmoFlags::kRead | fio::wire::VmoFlags::kExecute;
constexpr auto kLibDriverPath = "/pkg/driver/compat.so";

zx::result<zx::vmo> LoadVmo(fdf::Namespace& ns, const char* path,
                            fuchsia_io::wire::OpenFlags flags) {
  zx::result file = ns.Open<fuchsia_io::File>(path, flags);
  if (file.is_error()) {
    return file.take_error();
  }
  fidl::WireResult result = fidl::WireCall(file.value())->GetBackingMemory(kVmoFlags);
  if (!result.ok()) {
    return zx::error(result.status());
  }
  if (result.value().is_error()) {
    return result.value().take_error();
  }
  return zx::ok(std::move(result.value().value()->vmo));
}

zx::result<zx::resource> GetRootResource(fdf::Namespace& ns) {
  zx::result resource = ns.Connect<fboot::RootResource>();
  if (resource.is_error()) {
    return resource.take_error();
  }
  fidl::WireResult result = fidl::WireCall(resource.value())->Get();
  if (!result.ok()) {
    return zx::error(result.status());
  }
  return zx::ok(std::move(result.value().resource));
}

}  // namespace

namespace compat {

// This lock protects the global logger list.
std::mutex kGlobalLoggerListLock;

// This contains all the loggers in this driver host.
#ifdef DRIVER_COMPAT_ADD_NODE_NAMES_TO_LOG_TAGS
GlobalLoggerList global_logger_list __TA_GUARDED(kGlobalLoggerListLock)(true);
#else
GlobalLoggerList global_logger_list __TA_GUARDED(kGlobalLoggerListLock)(false);
#endif

zx_status_t AddMetadata(Device* device,
                        fidl::VectorView<fuchsia_driver_compat::wire::Metadata> data) {
  for (auto& metadata : data) {
    size_t size;
    zx_status_t status = metadata.data.get_property(ZX_PROP_VMO_CONTENT_SIZE, &size, sizeof(size));
    if (status != ZX_OK) {
      return status;
    }
    std::vector<uint8_t> data(size);
    status = metadata.data.read(data.data(), 0, data.size());
    if (status != ZX_OK) {
      return status;
    }

    status = device->AddMetadata(metadata.type, data.data(), data.size());
    if (status != ZX_OK) {
      return status;
    }
  }
  return ZX_OK;
}

promise<void, zx_status_t> GetAndAddMetadata(
    fidl::WireClient<fuchsia_driver_compat::Device>& client, Device* device) {
  bridge<void, zx_status_t> bridge;
  client->GetMetadata().Then(
      [device, completer = std::move(bridge.completer)](
          fidl::WireUnownedResult<fuchsia_driver_compat::Device::GetMetadata>& result) mutable {
        if (!result.ok()) {
          return;
        }
        auto* response = result.Unwrap();
        if (response->is_error()) {
          completer.complete_error(response->error_value());
          return;
        }
        zx_status_t status = AddMetadata(device, response->value()->metadata);
        if (status != ZX_OK) {
          completer.complete_error(status);
          return;
        }
        completer.complete_ok();
      });
  return bridge.consumer.promise_or(error(ZX_ERR_INTERNAL));
}

void GlobalLoggerList::LoggerInstances::Log(FuchsiaLogSeverity severity, const char* tag,
                                            const char* file, int line, const char* msg,
                                            va_list args) {
  std::lock_guard guard(kGlobalLoggerListLock);
  auto it = loggers_.begin();

  if (it == loggers_.end()) {
    LOGF(WARNING, "No logger available in this LoggerInstances. Using host logger.");
    driver_logger::GetLogger().VLogWrite(severity, tag, msg, args, file, line);
    return;
  }

  if (!log_node_names_) {
    (*it)->logvf(severity, tag, file, line, msg, args);
    return;
  }

  if (tag) {
    node_names_.push_back(tag);
  }

  (*it)->logvf(severity, node_names_, file, line, msg, args);

  if (tag) {
    node_names_.pop_back();
  }
}

zx_driver_t* GlobalLoggerList::LoggerInstances::ZxDriver() {
  return static_cast<zx_driver_t*>(this);
}

void GlobalLoggerList::LoggerInstances::AddLogger(std::shared_ptr<fdf::Logger>& logger,
                                                  const std::optional<std::string>& node_name) {
  loggers_.insert(logger);
  if (log_node_names_ && node_name.has_value()) {
    node_names_.push_back(node_name.value());
  }
}

void GlobalLoggerList::LoggerInstances::RemoveLogger(std::shared_ptr<fdf::Logger>& logger,
                                                     const std::optional<std::string>& node_name) {
  loggers_.erase(logger);
  if (log_node_names_ && node_name.has_value()) {
    node_names_.erase(std::remove(node_names_.begin(), node_names_.end(), node_name.value()),
                      node_names_.end());
  }
}

zx_driver_t* GlobalLoggerList::AddLogger(const std::string& driver_path,
                                         std::shared_ptr<fdf::Logger>& logger,
                                         const std::optional<std::string>& node_name) {
  auto& instances = instances_.try_emplace(driver_path, log_node_names_).first->second;
  instances.AddLogger(logger, node_name);
  return instances.ZxDriver();
}

void GlobalLoggerList::RemoveLogger(const std::string& driver_path,
                                    std::shared_ptr<fdf::Logger>& logger,
                                    const std::optional<std::string>& node_name) {
  auto it = instances_.find(driver_path);
  if (it != instances_.end()) {
    it->second.RemoveLogger(logger, node_name);
    // Don't erase the instance even if it becomes empty. There are some drivers that incorrectly
    // log after they have been destroyed. We want to make sure that the logger instance that we
    // put for them is kept around. The empty loggers will just cause it to log with the
    // driver host's logger.
  }
}

std::optional<size_t> GlobalLoggerList::loggers_count_for_testing(const std::string& driver_path) {
  auto it = instances_.find(driver_path);
  if (it != instances_.end()) {
    return (*it).second.count();
  }

  return std::nullopt;
}

Driver::Driver(fdf::DriverStartArgs start_args,
               fdf::UnownedSynchronizedDispatcher driver_dispatcher, device_t device,
               const zx_protocol_device_t* ops, std::string_view driver_path)
    : Base("compat", std::move(start_args), std::move(driver_dispatcher)),
      executor_(dispatcher()),
      driver_path_(driver_path),
      device_(device, ops, this, std::nullopt, nullptr, dispatcher()) {
  // Give the parent device the correct node.
  device_.Bind({std::move(node()), dispatcher()});
  // Call this so the parent device is in the post-init state.
  device_.InitReply(ZX_OK);
  ZX_ASSERT(url().has_value());
}

Driver::~Driver() {
  if (ShouldCallRelease()) {
    record_->ops->release(context_);
  }
  dlclose(library_);
  {
    std::lock_guard guard(kGlobalLoggerListLock);
    global_logger_list.RemoveLogger(driver_path(), inner_logger_, node_name());
  }
}

void Driver::Start(fdf::StartCompleter completer) {
  // Serve the diagnostics directory.
  if (zx_status_t status = ServeDiagnosticsDir(); status != ZX_OK) {
    completer(zx::error(status));
    return;
  }

  zx::result loader_vmo = LoadVmo(*incoming(), kLibDriverPath, kOpenFlags);
  if (loader_vmo.is_error()) {
    FDF_LOGL(ERROR, *logger_, "Failed to open loader vmo: %s", loader_vmo.status_string());
    completer(loader_vmo.take_error());
    return;
  }

  zx::result driver_vmo = LoadVmo(*incoming(), driver_path_.c_str(), kOpenFlags);
  if (driver_vmo.is_error()) {
    FDF_LOGL(ERROR, *logger_, "Failed to open driver vmo: %s", driver_vmo.status_string());
    completer(loader_vmo.take_error());
    return;
  }

  if (zx::result result = LoadDriver(std::move(loader_vmo.value()), std::move(driver_vmo.value()));
      result.is_error()) {
    FDF_LOGL(ERROR, *logger_, "Failed to load driver: %s", result.status_string());
    completer(result.take_error());
    return;
  }

  // Store start completer to be replied to later. It will either be done when the below promises
  // hit an error or after the init hook is replied to and the node has been created and a devfs
  // node has been exported.
  start_completer_.emplace(std::move(completer));

  auto start_driver =
      Driver::ConnectToParentDevices()
          .and_then(fit::bind_member<&Driver::GetDeviceInfo>(this))
          .then([this](result<void, zx_status_t>& result) -> fpromise::result<void, zx_status_t> {
            if (result.is_error()) {
              FDF_LOGL(WARNING, *logger_, "Getting DeviceInfo failed with: %s",
                       zx_status_get_string(result.error()));
            }
            if (zx::result result = StartDriver(); result.is_error()) {
              FDF_LOGL(ERROR, *logger_, "Failed to start driver '%s': %s", url().value().data(),
                       result.status_string());
              device_.Unbind();
              CompleteStart(result.take_error());
              return error(result.error_value());
            }
            return ok();
          })
          .wrap_with(scope_);
  executor_.schedule_task(std::move(start_driver));
}

bool Driver::IsComposite() { return !parent_clients_.empty(); }

zx_handle_t Driver::GetRootResource() {
  if (!root_resource_.is_valid()) {
    zx::result resource = ::GetRootResource(*incoming());
    if (resource.is_ok()) {
      root_resource_ = std::move(resource.value());
    } else {
      FDF_LOGL(WARNING, *logger_, "Failed to get root_resource '%s'", resource.status_string());
    }
  }
  return root_resource_.get();
}

bool Driver::IsRunningOnDispatcher() const {
  fdf::Unowned<fdf::Dispatcher> current_dispatcher = fdf::Dispatcher::GetCurrent();
  if (current_dispatcher == fdf::Unowned<fdf::Dispatcher>{}) {
    return false;
  }
  return current_dispatcher->async_dispatcher() == dispatcher();
}

zx_status_t Driver::RunOnDispatcher(fit::callback<zx_status_t()> task) {
  if (IsRunningOnDispatcher()) {
    return task();
  }

  libsync::Completion completion;
  zx_status_t task_status;
  auto discarded = fit::defer([&] {
    task_status = ZX_ERR_CANCELED;
    completion.Signal();
  });
  zx_status_t status =
      async::PostTask(dispatcher(), [&task_status, &completion, task = std::move(task),
                                     discarded = std::move(discarded)]() mutable {
        discarded.cancel();
        task_status = task();
        completion.Signal();
      });
  if (status != ZX_OK) {
    return status;
  }
  completion.Wait();
  return status;
}

void Driver::PrepareStop(fdf::PrepareStopCompleter completer) {
  zx::result client = this->incoming()->Connect<fuchsia_device_manager::SystemStateTransition>();
  if (client.is_error()) {
    FDF_SLOG(ERROR, "failed to connect to fuchsia.device.manager/SystemStateTransition",
             KV("status", client.status_value()));
    completer(client.take_error());
    return;
  }
  fidl::WireResult result = fidl::WireCall(client.value())->GetTerminationSystemState();
  if (!result.ok()) {
    FDF_SLOG(ERROR, "failed to get termination state", KV("status", client.status_value()));
    completer(zx::error(result.error().status()));
    return;
  }

  system_state_ = result->state;
  stop_triggered_ = true;

  executor_.schedule_task(device_.HandleStopSignal().then(
      [completer = std::move(completer)](fpromise::result<void>& init) mutable {
        completer(zx::ok());
      }));
}

zx::result<> Driver::LoadDriver(zx::vmo loader_vmo, zx::vmo driver_vmo) {
  const std::string& url_str = url().value();

  // Replace loader service to load the DFv1 driver, load the driver,
  // then place the original loader service back.
  {
    // This requires a lock because the loader is a global variable.
    std::scoped_lock lock(kDriverGlobalsLock);
    zx::result new_loader_endpoints = fidl::CreateEndpoints<fldsvc::Loader>();
    if (new_loader_endpoints.is_error()) {
      return new_loader_endpoints.take_error();
    }
    fidl::ClientEnd<fldsvc::Loader> original_loader(
        zx::channel(dl_set_loader_service(new_loader_endpoints->client.channel().release())));
    auto reset_loader = fit::defer([&original_loader]() {
      zx::channel l = zx::channel(dl_set_loader_service(original_loader.TakeChannel().release()));
    });

    // Start loader.
    async::Loop loader_loop(&kAsyncLoopConfigNeverAttachToThread);
    zx_status_t status = loader_loop.StartThread("loader-loop");
    if (status != ZX_OK) {
      FDF_LOGL(ERROR, *logger_,
               "Failed to load driver '%s', could not start thread for loader loop: %s",
               url_str.data(), zx_status_get_string(status));
      return zx::error(status);
    }
    Loader loader(loader_loop.dispatcher(), original_loader.borrow(), std::move(loader_vmo));
    fidl::BindServer(loader_loop.dispatcher(), std::move(new_loader_endpoints->server), &loader);

    // Open driver.
    library_ = dlopen_vmo(driver_vmo.get(), RTLD_NOW);
    if (library_ == nullptr) {
      FDF_LOGL(ERROR, *logger_, "Failed to load driver '%s', could not load library: %s",
               url_str.data(), dlerror());
      return zx::error(ZX_ERR_INTERNAL);
    }
  }

  // Load and verify symbols.
  auto note = static_cast<const zircon_driver_note_t*>(dlsym(library_, "__zircon_driver_note__"));
  if (note == nullptr) {
    FDF_LOGL(ERROR, *logger_, "Failed to load driver '%s', driver note not found", url_str.data());
    return zx::error(ZX_ERR_BAD_STATE);
  }
  driver_name_ = note->payload.name;
  FDF_LOGL(INFO, *logger_, "Loaded driver '%s'", note->payload.name);
  record_ = static_cast<zx_driver_rec_t*>(dlsym(library_, "__zircon_driver_rec__"));
  if (record_ == nullptr) {
    FDF_LOGL(ERROR, *logger_, "Failed to load driver '%s', driver record not found",
             url_str.data());
    return zx::error(ZX_ERR_BAD_STATE);
  }
  if (record_->ops == nullptr) {
    FDF_LOGL(ERROR, *logger_, "Failed to load driver '%s', missing driver ops", url_str.data());
    return zx::error(ZX_ERR_BAD_STATE);
  }
  if (record_->ops->version != DRIVER_OPS_VERSION) {
    FDF_LOGL(ERROR, *logger_, "Failed to load driver '%s', incorrect driver version",
             url_str.data());
    return zx::error(ZX_ERR_WRONG_TYPE);
  }
  if (record_->ops->bind == nullptr && record_->ops->create == nullptr) {
    FDF_LOGL(ERROR, *logger_, "Failed to load driver '%s', missing '%s'", url_str.data(),
             (record_->ops->bind == nullptr ? "bind" : "create"));
    return zx::error(ZX_ERR_BAD_STATE);
  }
  if (record_->ops->bind != nullptr && record_->ops->create != nullptr) {
    FDF_LOGL(ERROR, *logger_, "Failed to load driver '%s', both 'bind' and 'create' are defined",
             url_str.data());
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  // Create our logger.
  zx::result logger_result = fdf::Logger::Create(*incoming(), dispatcher(), note->payload.name);
  if (logger_result.is_error()) {
    return logger_result.take_error();
  }

  // Move the logger over into a shared_ptr instead of unique_ptr so we can pass it to the global
  // logging manager and compat::Device.
  inner_logger_ = std::shared_ptr<fdf::Logger>(logger_result.value().release());
  device_.set_logger(inner_logger_);
  {
    std::lock_guard guard(kGlobalLoggerListLock);
    record_->driver = global_logger_list.AddLogger(driver_path(), inner_logger_, node_name());
  }

  return zx::ok();
}

zx::result<> Driver::TryRunUnitTests() {
  auto getvar_bool = [this](const char* key, bool default_value) {
    zx::result value = GetVariable(key);
    if (value.is_error()) {
      return default_value;
    }
    if (*value == "0" || *value == "false" || *value == "off") {
      return false;
    }
    return true;
  };

  bool default_opt = getvar_bool("driver.tests.enable", false);
  auto variable_name = std::string("driver.") + driver_name_ + ".tests.enable";
  bool should_run_unittests = getvar_bool(variable_name.c_str(), default_opt);
  if (should_run_unittests && record_->ops->run_unit_tests != nullptr) {
    zx::channel test_input, test_output;
    zx_status_t status = zx::channel::create(0, &test_input, &test_output);
    ZX_ASSERT_MSG(status == ZX_OK, "zx::channel::create failed with %s",
                  zx_status_get_string(status));

    bool tests_passed =
        record_->ops->run_unit_tests(context_, device_.ZxDevice(), test_input.release());
    if (!tests_passed) {
      FDF_LOGL(ERROR, *logger_, "[  FAILED  ] %s", driver_path().c_str());
      return zx::error(ZX_ERR_BAD_STATE);
    }
    FDF_LOGL(INFO, *logger_, "[  PASSED  ] %s", driver_path().c_str());
  }
  return zx::ok();
}

zx::result<> Driver::StartDriver() {
  const std::string& url_str = url().value();
  if (record_->ops->init != nullptr) {
    // If provided, run init.
    zx_status_t status = record_->ops->init(&context_);
    if (status != ZX_OK) {
      FDF_LOGL(ERROR, *logger_, "Failed to load driver '%s', 'init' failed: %s", url_str.data(),
               zx_status_get_string(status));
      return zx::error(status);
    }
  }

  zx::result result = TryRunUnitTests();
  if (result.is_error()) {
    return result.take_error();
  }

  if (record_->ops->bind != nullptr) {
    // If provided, run bind and return.
    zx_status_t status = record_->ops->bind(context_, device_.ZxDevice());
    if (status != ZX_OK) {
      FDF_LOGL(ERROR, *logger_, "Failed to load driver '%s', 'bind' failed: %s", url_str.data(),
               zx_status_get_string(status));
      return zx::error(status);
    }
  } else {
    // Else, run create and return.
    auto client_end = incoming()->Connect<fboot::Items>();
    if (client_end.is_error()) {
      return zx::error(client_end.status_value());
    }
    zx_status_t status = record_->ops->create(context_, device_.ZxDevice(), "proxy",
                                              client_end->channel().release());
    if (status != ZX_OK) {
      FDF_LOGL(ERROR, *logger_, "Failed to load driver '%s', 'create' failed: %s", url_str.data(),
               zx_status_get_string(status));
      return zx::error(status);
    }
  }
  if (!device_.HasChildren()) {
    FDF_LOGL(ERROR, *logger_, "Driver '%s' did not add a child device", url_str.data());
    return zx::error(ZX_ERR_BAD_STATE);
  }
  return zx::ok();
}

fpromise::promise<void, zx_status_t> Driver::ConnectToParentDevices() {
  bridge<void, zx_status_t> bridge;
  compat::ConnectToParentDevices(
      dispatcher(), incoming().get(),
      [this, completer = std::move(bridge.completer)](
          zx::result<std::vector<compat::ParentDevice>> devices) mutable {
        if (devices.is_error()) {
          completer.complete_error(devices.error_value());
          return;
        }
        std::vector<std::string> parents_names;
        for (auto& device : devices.value()) {
          if (device.name == "default") {
            parent_client_ = fidl::WireClient<fuchsia_driver_compat::Device>(
                std::move(device.client), dispatcher());
            continue;
          }

          // TODO(fxbug.dev/100985): When services stop adding extra instances
          // separated by ',' then remove this check.
          if (device.name.find(',') != std::string::npos) {
            continue;
          }

          parents_names.push_back(device.name);
          parent_clients_[device.name] = fidl::WireClient<fuchsia_driver_compat::Device>(
              std::move(device.client), dispatcher());
        }
        device_.set_fragments(std::move(parents_names));
        completer.complete_ok();
      });
  return bridge.consumer.promise_or(error(ZX_ERR_INTERNAL)).wrap_with(scope_);
}

promise<void, zx_status_t> Driver::GetDeviceInfo() {
  if (!parent_client_) {
    return fpromise::make_result_promise<void, zx_status_t>(error(ZX_ERR_PEER_CLOSED));
  }

  std::vector<promise<void, zx_status_t>> promises;

  // Get our topological path from our default parent.
  bridge<void, zx_status_t> topo_bridge;
  parent_client_->GetTopologicalPath().Then(
      [this, completer = std::move(topo_bridge.completer)](
          fidl::WireUnownedResult<fuchsia_driver_compat::Device::GetTopologicalPath>&
              result) mutable {
        if (!result.ok()) {
          FDF_LOGL(ERROR, *logger_, "Failed to get topo path %s",
                   zx_status_get_string(result.status()));
          return;
        }
        auto* response = result.Unwrap();
        auto topological_path = std::string(response->path.data(), response->path.size());
        // If we are a composite then we have to add the name of our composite device
        // to our primary parent. The composite device's name is the node_name handed
        // to us.
        if (IsComposite()) {
          topological_path.append("/");
          topological_path.append(node_name().value());
        }
        device_.set_topological_path(std::move(topological_path));
        completer.complete_ok();
      });

  promises.push_back(topo_bridge.consumer.promise_or(error(ZX_ERR_INTERNAL)));

  // Get our metadata from our fragments if we are a composite,
  // or our primary parent.
  if (IsComposite()) {
    for (auto& client : parent_clients_) {
      promises.push_back(GetAndAddMetadata(client.second, &device_));
    }
  } else {
    promises.push_back(GetAndAddMetadata(parent_client_, &device_));
  }

  // Collect all our promises and return the first error we see.
  return join_promise_vector(std::move(promises))
      .then([](fpromise::result<std::vector<fpromise::result<void, zx_status_t>>>& results) {
        if (results.is_error()) {
          return fpromise::make_result_promise(error(ZX_ERR_INTERNAL));
        }
        for (auto& result : results.value()) {
          if (result.is_error()) {
            return fpromise::make_result_promise(error(result.error()));
          }
        }
        return fpromise::make_result_promise<void, zx_status_t>(ok());
      });
}

void* Driver::Context() const { return context_; }

zx::result<zx::vmo> Driver::LoadFirmware(Device* device, const char* filename, size_t* size) {
  std::string full_filename = "/pkg/lib/firmware/";
  full_filename.append(filename);
  fpromise::result connect_result = fpromise::run_single_threaded(
      fdf::Open(*incoming(), dispatcher(), full_filename.c_str(), kOpenFlags));
  if (connect_result.is_error()) {
    return zx::error(connect_result.take_error());
  }

  fidl::WireResult get_backing_memory_result =
      connect_result.take_value().sync()->GetBackingMemory(fio::wire::VmoFlags::kRead);
  if (!get_backing_memory_result.ok()) {
    if (get_backing_memory_result.is_peer_closed()) {
      return zx::error(ZX_ERR_NOT_FOUND);
    }
    return zx::error(get_backing_memory_result.status());
  }
  const auto* res = get_backing_memory_result.Unwrap();
  if (res->is_error()) {
    return zx::error(res->error_value());
  }
  zx::vmo& vmo = res->value()->vmo;
  if (zx_status_t status = vmo.get_prop_content_size(size); status != ZX_OK) {
    return zx::error(status);
  }
  return zx::ok(std::move(vmo));
}

zx_status_t Driver::AddDevice(Device* parent, device_add_args_t* args, zx_device_t** out) {
  return RunOnDispatcher([&] {
    zx_device_t* child;
    zx_status_t status = parent->Add(args, &child);
    if (status != ZX_OK) {
      FDF_LOGL(ERROR, *logger_, "Failed to add device %s: %s", args->name,
               zx_status_get_string(status));
      return status;
    }
    if (out) {
      *out = child;
    }
    return ZX_OK;
  });
}

zx::result<> Driver::SetProfileByRole(zx::unowned_thread thread, std::string_view role) {
  auto profile_client = incoming()->Connect<fuchsia_scheduler::ProfileProvider>();
  if (profile_client.is_error()) {
    return profile_client.take_error();
  }

  zx::thread duplicate_thread;
  zx_status_t status =
      thread->duplicate(ZX_RIGHT_TRANSFER | ZX_RIGHT_MANAGE_THREAD, &duplicate_thread);
  if (status != ZX_OK) {
    return zx::error(status);
  }

  fidl::WireResult result =
      fidl::WireCall(*profile_client)
          ->SetProfileByRole(std::move(duplicate_thread), fidl::StringView::FromExternal(role));
  if (!result.ok()) {
    return zx::error(result.status());
  }
  if (result->status != ZX_OK) {
    return zx::error(result->status);
  }
  return zx::ok();
}

zx::result<std::string> Driver::GetVariable(const char* name) {
  auto boot_args = incoming()->Connect<fuchsia_boot::Arguments>();
  if (boot_args.is_error()) {
    return boot_args.take_error();
  }

  auto result = fidl::WireCall(*boot_args)->GetString(fidl::StringView::FromExternal(name));
  if (!result.ok() || result->value.is_null() || result->value.empty()) {
    return zx::error(ZX_ERR_NOT_FOUND);
  }
  return zx::ok(std::string(result->value.data(), result->value.size()));
}

void DriverFactory::CreateDriver(fdf::DriverStartArgs start_args,
                                 fdf::UnownedSynchronizedDispatcher driver_dispatcher,
                                 fdf::StartCompleter completer) {
  auto compat_device = fdf_internal::GetSymbol<const device_t*>(start_args.symbols(), kDeviceSymbol,
                                                                &kDefaultDevice);
  const zx_protocol_device_t* ops =
      fdf_internal::GetSymbol<const zx_protocol_device_t*>(start_args.symbols(), kOps);

  // Open the compat driver's binary within the package.
  auto compat = fdf_internal::ProgramValue(start_args.program(), "compat");
  if (compat.is_error()) {
    completer(compat.take_error());
    return;
  }

  auto driver = std::make_unique<Driver>(std::move(start_args), std::move(driver_dispatcher),
                                         *compat_device, ops, "/pkg/" + *compat);
  fdf::DriverBase* driver_ptr = driver.get();
  completer.set_driver(std::move(driver));
  driver_ptr->Start(std::move(completer));
}

zx_status_t Driver::ServeDiagnosticsDir() {
  diagnostics_vfs_ = std::make_unique<fs::SynchronousVfs>(dispatcher());

  zx::result endpoints = fidl::CreateEndpoints<fio::Directory>();
  if (endpoints.is_error()) {
    return endpoints.status_value();
  }
  auto& [client, server] = endpoints.value();
  zx_status_t status = diagnostics_vfs_->ServeDirectory(diagnostics_dir_, std::move(server));
  if (status != ZX_OK) {
    FDF_LOGL(ERROR, *logger_, "Failed to serve diagnostics dir: %s", zx_status_get_string(status));
    return status;
  }
  zx::result result = outgoing().AddDirectory(std::move(client), "diagnostics");
  if (result.is_error()) {
    FDF_LOGL(ERROR, *logger_, "Failed to add diagnostics directory: %s", result.status_string());
    return result.status_value();
  }
  return ZX_OK;
}

zx_status_t Driver::GetFragmentProtocol(const char* fragment, uint32_t proto_id, void* out) {
  auto iter = parent_clients_.find(fragment);
  if (iter == parent_clients_.end()) {
    FDF_LOGL(ERROR, *logger_, "Failed to find compat client of fragment");
    return ZX_ERR_NOT_FOUND;
  }
  fidl::WireClient<fuchsia_driver_compat::Device>& client = iter->second;

  static uint64_t process_koid = []() {
    zx_info_handle_basic_t basic;
    ZX_ASSERT(zx::process::self()->get_info(ZX_INFO_HANDLE_BASIC, &basic, sizeof(basic), nullptr,
                                            nullptr) == ZX_OK);
    return basic.koid;
  }();

  fidl::WireResult result = client.sync()->GetBanjoProtocol(proto_id, process_koid);
  if (!result.ok()) {
    FDF_LOGL(ERROR, *logger_, "Failed to send request to get banjo protocol: %s",
             result.status_string());
    return result.status();
  }
  if (result->is_error()) {
    FDF_LOGL(ERROR, *logger_, "Failed to get banjo protocol: %s",
             zx_status_get_string(result->error_value()));
    return result->error_value();
  }

  struct GenericProtocol {
    const void* ops;
    void* ctx;
  };

  auto proto = static_cast<GenericProtocol*>(out);
  proto->ops = reinterpret_cast<const void*>(result->value()->ops);
  proto->ctx = reinterpret_cast<void*>(result->value()->context);
  return ZX_OK;
}

}  // namespace compat

using record = fdf_internal::Lifecycle<compat::Driver, compat::DriverFactory>;
FUCHSIA_DRIVER_LIFECYCLE_CPP_V3(record);
