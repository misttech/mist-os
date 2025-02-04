// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_host/driver.h"

#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async/cpp/task.h>
#include <lib/async_patterns/cpp/dispatcher_bound.h>
#include <lib/driver/component/cpp/internal/start_args.h>
#include <lib/fdf/cpp/dispatcher.h>
#include <lib/fdf/cpp/env.h>
#include <lib/fdio/directory.h>
#include <lib/fit/defer.h>
#include <lib/sync/cpp/completion.h>
#include <zircon/dlfcn.h>

#include <fbl/auto_lock.h>

#include "src/devices/bin/driver_host/loader.h"
#include "src/devices/lib/log/log.h"
#include "src/lib/driver_symbols/symbols.h"

namespace fdh = fuchsia_driver_host;
namespace fio = fuchsia_io;
namespace fldsvc = fuchsia_ldsvc;

namespace fdf {
using namespace fuchsia_driver_framework;
}  // namespace fdf

namespace driver_host {

namespace {

static constexpr std::string_view kCompatDriverRelativePath = "driver/compat.so";

std::string_view GetFilename(std::string_view path) {
  size_t index = path.rfind('/');
  return index == std::string_view::npos ? path : path.substr(index + 1);
}

std::string_view GetManifest(std::string_view url) { return GetFilename(url); }

class FileEventHandler final : public fidl::AsyncEventHandler<fio::File> {
 public:
  explicit FileEventHandler(std::string url) : url_(std::move(url)) {}

  void on_fidl_error(fidl::UnbindInfo info) override {
    LOGF(ERROR, "Failed to start driver '%s'; could not open library: %s", url_.c_str(),
         info.FormatDescription().c_str());
  }

  void handle_unknown_event(fidl::UnknownEventMetadata<fio::File>) override {}

 private:
  std::string url_;
};

zx::result<fidl::ClientEnd<fio::File>> OpenDriverFile(const fdf::DriverStartArgs& start_args,
                                                      std::string_view relative_binary_path) {
  const auto& incoming = start_args.incoming();
  auto pkg = incoming ? fdf_internal::NsValue(*incoming, "/pkg") : zx::error(ZX_ERR_INVALID_ARGS);
  if (pkg.is_error()) {
    LOGF(ERROR, "Failed to start driver, missing '/pkg' directory: %s", pkg.status_string());
    return pkg.take_error();
  }
  // Open the driver's binary within the driver's package.
  auto [client_end, server_end] = fidl::Endpoints<fio::File>::Create();
  zx_status_t status =
      fdio_open3_at(pkg->channel()->get(), relative_binary_path.data(),
                    static_cast<uint64_t>(fio::wire::kPermReadable | fio::wire::kPermExecutable),
                    server_end.TakeChannel().release());
  if (status != ZX_OK) {
    LOGF(ERROR, "Failed to start driver; could not open library: %s", zx_status_get_string(status));
    return zx::error(status);
  }
  return zx::ok(std::move(client_end));
}

// Parses out the module map from the start arg's program dictionary.
zx::result<ModuleMap> ParseModuleMap(const fuchsia_driver_framework::DriverStartArgs& start_args) {
  const std::string& url = *start_args.url();
  fidl::Arena arena;
  fuchsia_data::wire::Dictionary wire_program = fidl::ToWire(arena, *start_args.program());

  zx::result modules = fdf_internal::ProgramValueAsObjVector(wire_program, "modules");
  if (!modules.is_ok()) {
    return zx::ok(ModuleMap{});
  }
  ModuleMap module_map;
  for (const auto& wire_module : *modules) {
    zx::result<std::string> module_name = fdf_internal::ProgramValue(wire_module, "module_name");
    if (module_name.is_error()) {
      LOGF(ERROR, "Failed to start driver, missing 'module_name' argument: %s",
           module_name.status_string());
      return module_name.take_error();
    }

    // Special case for compat. The syntax could allow more more generic references to other
    // fields, but we don't need that for now, so we hardcode support for one specific field.
    if (*module_name == "#program.compat") {
      module_name = fdf_internal::ProgramValue(wire_program, "compat");
      if (module_name.is_error()) {
        LOGF(ERROR, "Failed to start driver, missing 'compat' argument: %s",
             module_name.status_string());
        return module_name.take_error();
      }
    }

    zx::result module_file = OpenDriverFile(start_args, *module_name);
    if (module_file.is_error()) {
      LOGF(ERROR, "Failed to open driver '%s' module file: %s", url.c_str(),
           module_file.status_string());
      return module_file.take_error();
    }

    // Call GetBackingMemory for the module file.
    auto module_vmo = fidl::Call(*module_file)
                          ->GetBackingMemory(fio::VmoFlags::kRead | fio::VmoFlags::kExecute |
                                             fio::VmoFlags::kPrivateClone);
    if (!module_vmo.is_ok()) {
      LOGF(ERROR, "Failed to open driver '%s' module VMO: %s", url.c_str(),
           module_vmo.error_value().FormatDescription().c_str());
      zx_status_t status = module_vmo.error_value().is_domain_error()
                               ? module_vmo.error_value().domain_error()
                               : module_vmo.error_value().framework_error().status();
      return zx::error(status);
    }

    // Lookup overrides specific to this module.
    OverrideMap overrides;
    zx::result wire_overrides =
        fdf_internal::ProgramValueAsObjVector(wire_module, "loader_overrides");
    if (wire_overrides.is_ok()) {
      for (const auto& wire_override : wire_overrides.value()) {
        zx::result<std::string> from = fdf_internal::ProgramValue(wire_override, "from");
        if (from.is_error()) {
          LOGF(ERROR, "Failed to start driver, missing 'from' argument: %s", from.status_string());
          return from.take_error();
        }

        zx::result<std::string> to = fdf_internal::ProgramValue(wire_override, "to");
        if (to.is_error()) {
          LOGF(ERROR, "Failed to start driver, missing 'to' argument: %s", to.status_string());
          return to.take_error();
        }

        zx::result override_file = OpenDriverFile(start_args, *to);
        if (override_file.is_error()) {
          LOGF(ERROR, "Failed to open driver '%s' override file: %s", url.c_str(),
               module_file.status_string());
          return module_file.take_error();
        }
        overrides.emplace(*from, std::move(*override_file));
      }
    }

    // Lookup symbols specific to this module.
    zx::result symbols = fdf_internal::ProgramValueAsVector(wire_module, "symbols");
    if (symbols.is_error()) {
      LOGF(ERROR, "Failed to start driver, missing 'symbols' argument: %s",
           symbols.status_string());
      return symbols.take_error();
    }

    module_map.emplace(*module_name, Module{
                                         .module_vmo = std::move(module_vmo->vmo()),
                                         .overrides = std::move(overrides),
                                         .symbols = std::move(*symbols),
                                     });
  }
  return zx::ok(std::move(module_map));
}
struct ModulesAndSymbols {
  std::vector<void*> modules;
  std::vector<Symbol> symbols;
};

zx::result<ModulesAndSymbols> LoadModulesAndSymbols(const std::string& url, ModuleMap modules) {
  async::Loop loader_loop(&kAsyncLoopConfigNoAttachToCurrentThread);
  zx_status_t status = loader_loop.StartThread("loader-loop");
  if (status != ZX_OK) {
    LOGF(ERROR, "Failed to load driver '%s', could not start thread for loader loop: %s",
         url.c_str(), zx::make_result(status).status_string());
    return zx::error(status);
  }

  // For each module:
  // * Replace loader service to load the modules
  // * Load the module
  // * Get symbols
  // * Place the original loader service back
  ModulesAndSymbols modules_and_symbols;
  for (auto& [module_name, module_] : modules) {
    void* module_library = nullptr;

    if (!module_.overrides.empty()) {
      auto [client_end, server_end] = fidl::Endpoints<fldsvc::Loader>::Create();
      fidl::ClientEnd<fldsvc::Loader> original_loader(
          zx::channel(dl_set_loader_service(client_end.channel().release())));
      auto reset_loader = fit::defer([&original_loader]() {
        zx::channel(dl_set_loader_service(original_loader.TakeChannel().release()));
      });

      // Start loader.
      async_patterns::DispatcherBound<Loader> loader(loader_loop.dispatcher(), std::in_place,
                                                     original_loader.borrow(),
                                                     std::move(module_.overrides));
      loader.AsyncCall(&Loader::Bind, std::move(server_end));
      // Ensure the Bind call completes. It will assert if the loop is shutdown before the fidl Bind
      // completes. Posting a task ensures all previously posted tasks are completed as tasks are
      // executed in fifo order.
      libsync::Completion completion;
      async::PostTask(loader_loop.dispatcher(), [&completion]() { completion.Signal(); });
      completion.Wait();
      // Open module.
      module_library = dlopen_vmo(module_.module_vmo.get(), RTLD_NOW);
      if (module_library == nullptr) {
        LOGF(ERROR, "Failed to load driver '%s', could not load module: %s", url.c_str(),
             dlerror());
        return zx::error(ZX_ERR_INTERNAL);
      }
    } else {
      // Open module.
      module_library = dlopen_vmo(module_.module_vmo.get(), RTLD_NOW);
      if (module_library == nullptr) {
        LOGF(ERROR, "Failed to load driver '%s', could not load module: %s", url.c_str(),
             dlerror());
        return zx::error(ZX_ERR_INTERNAL);
      }
    }

    // Find symbols.
    for (const auto& symbol_name : module_.symbols) {
      void* symbol = dlsym(module_library, symbol_name.c_str());
      if (symbol == nullptr) {
        LOGF(ERROR, "Failed to load driver '%s', module symbol '%s' not found", url.c_str(),
             symbol);
        return zx::error(ZX_ERR_BAD_STATE);
      }
      modules_and_symbols.symbols.emplace_back(Symbol{
          .module_name = module_name,
          .symbol_name = symbol_name,
          .address = symbol,
      });
    }

    modules_and_symbols.modules.emplace_back(module_library);
  }
  return zx::ok(modules_and_symbols);
}

}  // namespace

zx::result<fbl::RefPtr<Driver>> Driver::Load(std::string url, zx::vmo vmo,
                                             std::string_view relative_binary_path,
                                             ModuleMap modules) {
  // Give the driver's VMO a name.
  std::string_view vmo_name = GetFilename(relative_binary_path);
  zx_status_t status = vmo.set_property(ZX_PROP_NAME, vmo_name.data(), vmo_name.size());
  if (status != ZX_OK) {
    LOGF(ERROR, "Failed to start driver '%s', could not name library VMO '%s': %s", url.c_str(),
         std::string(vmo_name).c_str(), zx_status_get_string(status));
    return zx::error(status);
  }

  // If we are using the compat shim, we do symbol validation when loading the DFv1 driver vmo.
  // This is as here the |vmo| will correspond to the compat driver, but the |url| will be the
  // DFv1 driver's url, so we would be incorrectly looking for |url| in the allowlist for
  // the compat driver's symbols.
  if (relative_binary_path != kCompatDriverRelativePath) {
    auto result = driver_symbols::FindRestrictedSymbols(vmo, url);
    if (result.is_error()) {
      LOGF(WARNING, "Driver '%s' failed to validate as ELF: %s", url.c_str(),
           result.status_value());
    } else if (result->size() > 0) {
      LOGF(ERROR, "Driver '%s' referenced %lu restricted libc symbols: ", url.c_str(),
           result->size());
      for (auto& str : *result) {
        LOGF(ERROR, str.c_str());
      }
      return zx::error(ZX_ERR_NOT_SUPPORTED);
    }
  }

  void* library = dlopen_vmo(vmo.get(), RTLD_NOW);
  if (library == nullptr) {
    LOGF(ERROR, "Failed to start driver '%s', could not load library: %s", url.data(), dlerror());
    return zx::error(ZX_ERR_INTERNAL);
  }

  auto registration =
      static_cast<const DriverRegistration*>(dlsym(library, "__fuchsia_driver_registration__"));

  if (registration == nullptr) {
    LOGF(
        ERROR,
        "Failed to start driver '%s': __fuchsia_driver_registration__ symbol not available, does the driver have FUCHSIA_DRIVER_EXPORT?",
        url.data());
    return zx::error(ZX_ERR_NOT_FOUND);
  }

  if (registration->version < 1 || registration->version > DRIVER_REGISTRATION_VERSION_MAX) {
    LOGF(ERROR, "Failed to start driver '%s', unknown driver registration version: %lu", url.data(),
         registration->version);
    return zx::error(ZX_ERR_WRONG_TYPE);
  }

  zx::result result = LoadModulesAndSymbols(url, std::move(modules));
  if (result.is_error()) {
    return result.take_error();
  }

  return zx::ok(fbl::MakeRefCounted<Driver>(std::move(url), library, std::move(result->modules),
                                            std::move(result->symbols), registration));
}

Driver::Driver(std::string url, void* library, std::vector<void*> modules,
               std::vector<Symbol> symbols, DriverHooks hooks)
    : url_(std::move(url)),
      library_(library),
      modules_(std::move(modules)),
      symbols_(std::move(symbols)),
      hooks_(hooks) {}

Driver::~Driver() {
  if (token_.has_value()) {
    hooks_->v1.destroy(token_.value());
  } else {
    LOGF(WARNING, "Failed to Destroy driver '%s', initialize was not completed.", url_.c_str());
  }

  dlclose(library_);
  for (const auto& module_ : modules_) {
    dlclose(module_);
  }
}

void Driver::set_binding(fidl::ServerBindingRef<fdh::Driver> binding) {
  fbl::AutoLock al(&lock_);
  binding_.emplace(std::move(binding));
}

void Driver::Stop(StopCompleter::Sync& completer) {
  fbl::AutoLock al(&lock_);
  if (driver_client_.has_value()) {
    driver_client_.value().AsyncCall(&DriverClient::Stop);
  } else {
    LOGF(ERROR, "The driver_client_ is not available.");
  }
}

void Driver::Start(fbl::RefPtr<Driver> self, fdf::DriverStartArgs start_args,
                   ::fdf::Dispatcher dispatcher, fit::callback<void(zx::result<>)> cb) {
  if (!start_args.symbols().has_value()) {
    start_args.symbols(std::vector<fdf::NodeSymbol>());
  }
  for (const Symbol& symbol : symbols_) {
    start_args.symbols()->emplace_back(fdf::NodeSymbol{{
        .name = symbol.symbol_name,
        .address = reinterpret_cast<uint64_t>(symbol.address),
        .module_name = symbol.module_name,
    }});
  }

  fbl::AutoLock al(&lock_);
  initial_dispatcher_ = std::move(dispatcher);

  zx::result endpoints = fdf::CreateEndpoints<fuchsia_driver_framework::Driver>();
  if (endpoints.is_error()) {
    cb(endpoints.take_error());
    return;
  }

  async::PostTask(initial_dispatcher_.async_dispatcher(),
                  [this, hooks = hooks_, server = std::move(endpoints->server)]() mutable {
                    void* token = hooks->v1.initialize(server.TakeHandle().release());
                    fbl::AutoLock al(&lock_);
                    token_.emplace(token);
                  });

  zx::result client_dispatcher_result = fdf_env::DispatcherBuilder::CreateSynchronizedWithOwner(
      this, {}, "fdf-driver-client-dispatcher",
      [](fdf_dispatcher_t* dispatcher) { fdf_dispatcher_destroy(dispatcher); });

  ZX_ASSERT_MSG(client_dispatcher_result.is_ok(), "Failed to create driver client dispatcher: %s",
                client_dispatcher_result.status_string());

  client_dispatcher_ = std::move(client_dispatcher_result.value());
  driver_client_.emplace(client_dispatcher_.async_dispatcher(), std::in_place, self, url_);
  driver_client_.value().AsyncCall(&DriverClient::Bind, std::move(endpoints->client));
  driver_client_.value().AsyncCall(&DriverClient::Start, std::move(start_args), std::move(cb));
}

void Driver::ShutdownClient() {
  fbl::AutoLock al(&lock_);
  driver_client_.reset();
  client_dispatcher_.ShutdownAsync();
  // client_dispatcher_ will destroy itself in the shutdown completion callback.
  client_dispatcher_.release();
}

void Driver::Unbind() {
  fbl::AutoLock al(&lock_);
  if (binding_.has_value()) {
    // The binding's unbind handler will begin shutting down all dispatchers belonging to this
    // driver and when that is complete, it will remove this driver from its list causing this to
    // destruct.
    binding_->Unbind();

    // ServerBindingRef does not have ownership so resetting this is fine to avoid calling Unbind
    // multiple times.
    binding_.reset();
  }
}

uint32_t ExtractDefaultDispatcherOpts(const fuchsia_data::wire::Dictionary& program) {
  auto default_dispatcher_opts =
      fdf_internal::ProgramValueAsVector(program, "default_dispatcher_opts");

  uint32_t opts = 0;
  if (default_dispatcher_opts.is_ok()) {
    for (const auto& opt : *default_dispatcher_opts) {
      if (opt == "allow_sync_calls") {
        opts |= FDF_DISPATCHER_OPTION_ALLOW_SYNC_CALLS;
      } else {
        LOGF(WARNING, "Ignoring unknown default_dispatcher_opt: %s", opt.c_str());
      }
    }
  }
  return opts;
}

zx::result<fdf::Dispatcher> CreateDispatcher(const fbl::RefPtr<Driver>& driver,
                                             uint32_t dispatcher_opts, std::string scheduler_role) {
  auto name = GetManifest(driver->url());
  // The dispatcher must be shutdown before the dispatcher is destroyed.
  // Usually we will wait for the callback from |fdf_env::DriverShutdown| before destroying
  // the driver object (and hence the dispatcher).
  // In the case where we fail to start the driver, the driver object would be destructed
  // immediately, so here we hold an extra reference to the driver object to ensure the
  // dispatcher will not be destructed until shutdown completes.
  //
  // We do not destroy the dispatcher in the shutdown callback, to prevent crashes that
  // would happen if the driver attempts to access the dispatcher in its Stop hook.
  //
  // Currently we only support synchronized dispatchers for the default dispatcher.
  return fdf_env::DispatcherBuilder::CreateSynchronizedWithOwner(
      driver.get(), fdf::SynchronizedDispatcher::Options{.value = dispatcher_opts},
      std::format("{}-default-{:p}", name, static_cast<void*>(driver.get())),
      [driver_ref = driver](fdf_dispatcher_t* dispatcher) {}, scheduler_role);
}

void LoadDriver(fuchsia_driver_framework::DriverStartArgs start_args,
                async_dispatcher_t* dispatcher,
                fit::callback<void(zx::result<LoadedDriver>)> callback) {
  if (!start_args.url()) {
    LOGF(ERROR, "Failed to start driver, missing 'url' argument");
    callback(zx::error(ZX_ERR_INVALID_ARGS));
    return;
  }
  if (!start_args.program().has_value()) {
    LOGF(ERROR, "Failed to start driver, missing 'program' argument");
    callback(zx::error(ZX_ERR_INVALID_ARGS));
    return;
  }
  const std::string& url = *start_args.url();
  fidl::Arena arena;
  fuchsia_data::wire::Dictionary wire_program = fidl::ToWire(arena, *start_args.program());

  zx::result<std::string> binary = fdf_internal::ProgramValue(wire_program, "binary");
  if (binary.is_error()) {
    LOGF(ERROR, "Failed to start driver, missing 'binary' argument: %s", binary.status_string());
    callback(binary.take_error());
    return;
  }

  auto driver_file = OpenDriverFile(start_args, *binary);
  if (driver_file.is_error()) {
    LOGF(ERROR, "Failed to open driver '%s' file: %s", url.c_str(), driver_file.status_string());
    callback(driver_file.take_error());
    return;
  }

  uint32_t default_dispatcher_opts = driver_host::ExtractDefaultDispatcherOpts(wire_program);
  std::string default_dispatcher_scheduler_role = "";
  {
    auto scheduler_role =
        fdf_internal::ProgramValue(wire_program, "default_dispatcher_scheduler_role");
    if (scheduler_role.is_ok()) {
      default_dispatcher_scheduler_role = *scheduler_role;
    } else if (scheduler_role.status_value() != ZX_ERR_NOT_FOUND) {
      LOGF(ERROR, "Failed to parse scheduler role: %s", scheduler_role.status_string());
    }
  }

  // Pre-load all modules and overrides.
  zx::result<ModuleMap> modules = ParseModuleMap(start_args);
  if (modules.is_error()) {
    callback(modules.take_error());
    return;
  }

  // Once we receive the VMO from the call to GetBackingMemory, we can load the driver into this
  // driver host. We move the storage and encoded for start_args into this callback to extend its
  // lifetime.
  fidl::SharedClient file(std::move(*driver_file), dispatcher,
                          std::make_unique<FileEventHandler>(url));
  auto vmo_callback =
      [start_args = std::move(start_args), default_dispatcher_opts,
       default_dispatcher_scheduler_role, callback = std::move(callback),
       relative_binary_path = *binary, _ = file.Clone(),
       modules = std::move(*modules)](fidl::Result<fio::File::GetBackingMemory>& result) mutable {
        const std::string& url = *start_args.url();
        if (!result.is_ok()) {
          LOGF(ERROR, "Failed to start driver '%s', could not get library VMO: %s", url.c_str(),
               result.error_value().FormatDescription().c_str());
          zx_status_t status = result.error_value().is_domain_error()
                                   ? result.error_value().domain_error()
                                   : result.error_value().framework_error().status();
          callback(zx::error(status));
          return;
        }
        auto driver =
            Driver::Load(url, std::move(result->vmo()), relative_binary_path, std::move(modules));
        if (driver.is_error()) {
          LOGF(ERROR, "Failed to start driver '%s', could not Load driver: %s", url.c_str(),
               driver.status_string());
          callback(driver.take_error());
          return;
        }

        zx::result<fdf::Dispatcher> driver_dispatcher =
            CreateDispatcher(*driver, default_dispatcher_opts, default_dispatcher_scheduler_role);
        if (driver_dispatcher.is_error()) {
          LOGF(ERROR, "Failed to start driver '%s', could not create dispatcher: %s", url.c_str(),
               driver_dispatcher.status_string());
          callback(driver_dispatcher.take_error());
          return;
        }

        callback(zx::ok(LoadedDriver{
            .driver = std::move(*driver),
            .start_args = std::move(start_args),
            .dispatcher = std::move(*driver_dispatcher),
        }));
      };
  file->GetBackingMemory(fio::VmoFlags::kRead | fio::VmoFlags::kExecute |
                         fio::VmoFlags::kPrivateClone)
      .ThenExactlyOnce(std::move(vmo_callback));
}

}  // namespace driver_host
