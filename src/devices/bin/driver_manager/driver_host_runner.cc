// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/driver_host_runner.h"

#include <fidl/fuchsia.process/cpp/wire.h>
#include <lib/async/cpp/task.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/driver/component/cpp/internal/start_args.h>
#include <lib/fdio/directory.h>
#include <lib/fidl/cpp/wire/server.h>
#include <lib/fidl/cpp/wire/wire_messaging.h>
#include <zircon/errors.h>
#include <zircon/processargs.h>
#include <zircon/rights.h>
#include <zircon/status.h>

#include <random>

#include "src/devices/bin/driver_loader/loader.h"
#include "src/devices/lib/log/log.h"

namespace fio = fuchsia_io;
namespace fprocess = fuchsia_process;
namespace frunner = fuchsia_component_runner;
namespace fcomponent = fuchsia_component;
namespace fdecl = fuchsia_component_decl;

namespace driver_manager {

namespace {

constexpr uint32_t kTokenId = PA_HND(PA_USER0, 0);

zx::result<zx_koid_t> GetKoid(zx::unowned_handle handle) {
  zx_info_handle_basic_t info{};
  if (zx_status_t status =
          handle->get_info(ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr);
      status != ZX_OK) {
    return zx::error(status);
  }
  return zx::ok(info.koid);
}

const char* GetErrorString(fcomponent::Error error) {
  switch (error) {
    case fcomponent::Error::kInternal:
      return "INTERNAL";
    case fcomponent::Error::kInvalidArguments:
      return "INVALID_ARGUMENTS";
    case fcomponent::Error::kUnsupported:
      return "UNSUPPORTED";
    case fcomponent::Error::kAccessDenied:
      return "ACCESS_DENIED";
    case fcomponent::Error::kInstanceNotFound:
      return "INSTANCE_NOT_FOUND";
    case fcomponent::Error::kInstanceAlreadyExists:
      return "INSTANCE_ALREADY_EXISTS";
    case fcomponent::Error::kInstanceCannotStart:
      return "INSTANCE_CANNOT_START";
    case fcomponent::Error::kInstanceCannotResolve:
      return "INSTANCE_CANNOT_RESOLVE";
    case fcomponent::Error::kCollectionNotFound:
      return "COLLECTION_NOT_FOUND";
    case fcomponent::Error::kResourceUnavailable:
      return "RESOURCE_UNAVAILABLE";
    case fcomponent::Error::kInstanceDied:
      return "INSTANCE_DIED";
    case fcomponent::Error::kResourceNotFound:
      return "RESOURCE_NOT_FOUND";
    case fcomponent::Error::kInstanceCannotUnresolve:
      return "INSTANCE_CANNOT_UNRESOLVE";
    case fcomponent::Error::kInstanceAlreadyStarted:
      return "INSTANCE_ALREADY_STARTED";
    default:
      return "UNKNOWN_ERROR";
  }
}

zx::result<fidl::ClientEnd<fio::File>> OpenPkgFile(
    const std::vector<fuchsia_component_runner::ComponentNamespaceEntry>& incoming,
    std::string_view relative_binary_path) {
  auto pkg = fdf_internal::NsValue(incoming, "/pkg");
  if (pkg.is_error()) {
    LOGF(ERROR, "Failed to start driver host, missing '/pkg' directory: %s", pkg.status_string());
    return pkg.take_error();
  }
  // Open the driver's binary within the driver's package.
  auto [client_end, server_end] = fidl::Endpoints<fio::File>::Create();
  zx_status_t status = fdio_open_at(
      pkg->channel()->get(), relative_binary_path.data(),
      static_cast<uint32_t>(fio::OpenFlags::kRightReadable | fio::OpenFlags::kRightExecutable),
      server_end.TakeChannel().release());
  if (status != ZX_OK) {
    LOGF(ERROR, "Failed to start driver host; could not open library: %s",
         zx_status_get_string(status));
    return zx::error(status);
  }
  return zx::ok(std::move(client_end));
}

// TODO(https://fxbug.dev/341358132): support retrieving different vdsos. For now we will
// just use the driver manager's vdso.
zx::result<zx::vmo> GetVdsoVmo() {
  static const zx::vmo vdso{zx_take_startup_handle(PA_HND(PA_VMO_VDSO, 0))};
  zx::vmo copy;
  zx_status_t status = vdso.duplicate(ZX_RIGHT_SAME_RIGHTS, &copy);
  if (status != ZX_OK) {
    return zx::error(status);
  }
  return zx::ok(std::move(copy));
}

}  // namespace

DriverHostRunner::DriverHostRunner(async_dispatcher_t* dispatcher,
                                   fidl::ClientEnd<fcomponent::Realm> realm,
                                   std::unique_ptr<driver_loader::Loader> loader)
    : dispatcher_(dispatcher),
      realm_(fidl::WireClient(std::move(realm), dispatcher)),
      loader_(std::move(loader)) {
  // Pick a non-zero starting id so that folks cannot rely on the driver host process names being
  // stable.
  std::random_device rd;
  std::mt19937 gen(rd());
  std::uniform_int_distribution<> distrib(0, 1000);
  next_driver_host_id_ = distrib(gen);
}

void DriverHostRunner::PublishComponentRunner(component::OutgoingDirectory& outgoing) {
  auto result = outgoing.AddUnmanagedProtocol<frunner::ComponentRunner>(
      bindings_.CreateHandler(this, dispatcher_, fidl::kIgnoreBindingClosure),
      "fuchsia.component.runner.DriverHostRunner");
  ZX_ASSERT_MSG(result.is_ok(), "%s", result.status_string());
}

zx::result<DriverHostRunner::DriverHost*> DriverHostRunner::CreateDriverHost(
    std::string_view name) {
  zx::process process;
  zx::thread thread;
  zx::vmar root_vmar;
  zx_status_t status =
      zx::process::create(*zx::job::default_job(), name.data(), static_cast<uint32_t>(name.size()),
                          0, &process, &root_vmar);
  if (status != ZX_OK) {
    return zx::error(status);
  }
  status = zx::thread::create(process, name.data(), static_cast<uint32_t>(name.size()), 0, &thread);
  if (status != ZX_OK) {
    return zx::error(status);
  }
  auto driver_host =
      std::make_unique<DriverHost>(std::move(process), std::move(thread), std::move(root_vmar));
  DriverHost* driver_host_ptr = driver_host.get();
  driver_hosts_.push_back(std::move(driver_host));
  return zx::ok(driver_host_ptr);
}

zx_status_t DriverHostRunner::DriverHost::GetDuplicateHandles(zx::process* out_process,
                                                              zx::thread* out_thread,
                                                              zx::vmar* out_root_vmar) {
  zx::process process;
  zx::thread thread;
  zx::vmar root_vmar;
  zx_status_t status = process_.duplicate(ZX_RIGHT_SAME_RIGHTS, &process);
  if (status != ZX_OK) {
    LOGF(ERROR, "Failed to duplicate process handle: %s", zx_status_get_string(status));
    return status;
  }
  status = thread_.duplicate(ZX_RIGHT_SAME_RIGHTS, &thread);
  if (status != ZX_OK) {
    LOGF(ERROR, "Failed to duplicate thread handle: %s", zx_status_get_string(status));
    return status;
  }
  status = root_vmar_.duplicate(ZX_RIGHT_SAME_RIGHTS, &root_vmar);
  if (status != ZX_OK) {
    LOGF(ERROR, "Failed to duplicate vmar handle: %s", zx_status_get_string(status));
    return status;
  }
  *out_process = std::move(process);
  *out_thread = std::move(thread);
  *out_root_vmar = std::move(root_vmar);
  return ZX_OK;
}

zx::result<> DriverHostRunner::StartDriverHost() {
  constexpr std::string_view kUrl = "fuchsia-boot:///driver_host2#meta/driver_host2.cm";
  std::string name = "driver-host-new-" + std::to_string(next_driver_host_id_++);

  StartDriverHostComponent(
      name, kUrl,
      [this,
       name](zx::result<driver_manager::DriverHostRunner::StartedComponent> component) mutable {
        if (component.is_error()) {
          LOGF(ERROR, "Failed to start driver host: %s", component.status_string());
          return;
        }
        LoadDriverHost(component->info, name);
      });
  return zx::ok();
}

std::unordered_set<const DriverHostRunner::DriverHost*> DriverHostRunner::DriverHosts() {
  std::unordered_set<const DriverHostRunner::DriverHost*> result_hosts;
  for (auto& host : driver_hosts_) {
    result_hosts.insert(&host);
  }
  return result_hosts;
}

void DriverHostRunner::LoadDriverHost(
    const fuchsia_component_runner::ComponentStartInfo& start_info, std::string_view name) {
  auto url = *start_info.resolved_url();
  fidl::Arena arena;
  fuchsia_data::wire::Dictionary wire_program = fidl::ToWire(arena, *start_info.program());

  zx::result<std::string> binary = fdf_internal::ProgramValue(wire_program, "binary");
  if (binary.is_error()) {
    LOGF(ERROR, "Failed to start driver host, missing 'binary' argument: %s",
         binary.status_string());
    return;
  }

  auto driver_file = OpenPkgFile(*start_info.ns(), *binary);
  if (driver_file.is_error()) {
    LOGF(ERROR, "Failed to open driver host '%s' file: %s", url.c_str(),
         driver_file.status_string());
    return;
  }

  fidl::SyncClient file(std::move(*driver_file));
  auto result = file->GetBackingMemory(fio::VmoFlags::kRead | fio::VmoFlags::kExecute |
                                       fio::VmoFlags::kPrivateClone);

  if (result.is_error()) {
    LOGF(ERROR, "Failed to get driver host vmo: %s",
         zx_status_get_string(result.error_value().is_domain_error()
                                  ? result.error_value().domain_error()
                                  : ZX_ERR_BAD_HANDLE));
    return;
  }

  zx::vmo exec_vmo = std::move(result->vmo());

  auto vdso_result = GetVdsoVmo();
  if (vdso_result.is_error()) {
    LOGF(ERROR, "Failed to get vdso vmo, %s", vdso_result.status_string());
    return;
  }
  zx::vmo vdso_vmo = std::move(*vdso_result);

  zx::result<DriverHost*> driver_host = CreateDriverHost(name);
  if (driver_host.is_error()) {
    LOGF(ERROR, "Failed to create driver host env: %s", driver_host.status_string());
    return;
  }

  zx::process process;
  zx::thread thread;
  zx::vmar root_vmar;

  zx_status_t status = (*driver_host)->GetDuplicateHandles(&process, &thread, &root_vmar);
  if (status != ZX_OK) {
    LOGF(ERROR, "GetDuplicateHandles failed: %s", zx_status_get_string(status));
    return;
  }

  status = loader_->Start(std::move(process), std::move(thread), std::move(root_vmar),
                          std::move(exec_vmo), std::move(vdso_vmo));
  if (status != ZX_OK) {
    LOGF(ERROR, "Loader failed to start driver host: %lu", zx_status_get_string(status));
    return;
  }
}

void DriverHostRunner::StartDriverHostComponent(std::string_view moniker, std::string_view url,
                                                StartCallback callback) {
  zx::event token;
  zx_status_t status = zx::event::create(0, &token);
  if (status != ZX_OK) {
    return callback(zx::error(status));
  }

  zx::result koid = GetKoid(zx::unowned_handle(token.get()));
  if (koid.is_error()) {
    return callback(koid.take_error());
  }
  start_requests_.emplace(koid.value(), std::move(callback));

  fidl::Arena arena;
  auto child_decl = fdecl::wire::Child::Builder(arena)
                        .name(fidl::StringView::FromExternal(moniker))
                        .url(fidl::StringView::FromExternal(url))
                        .startup(fdecl::wire::StartupMode::kLazy)
                        .Build();

  fprocess::wire::HandleInfo handle_info = {
      .handle = std::move(token),
      .id = kTokenId,
  };

  auto child_args_builder = fcomponent::wire::CreateChildArgs::Builder(arena).numbered_handles(
      fidl::VectorView<fprocess::wire::HandleInfo>::FromExternal(&handle_info, 1));
  auto create_callback =
      [this, child_moniker = std::string(moniker.data()), koid = koid.value()](
          fidl::WireUnownedResult<fcomponent::Realm::CreateChild>& result) mutable {
        bool is_error = false;
        if (!result.ok()) {
          LOGF(ERROR, "Failed to create child '%s': %s", child_moniker.c_str(),
               result.FormatDescription().c_str());
          is_error = true;
        }
        if (result.value().is_error()) {
          LOGF(ERROR, "Failed to create child '%s': %s", child_moniker.c_str(),
               GetErrorString(result.value().error_value()));
          is_error = true;
        }
        if (is_error) {
          zx::result result = CallCallback(koid, zx::error(ZX_ERR_INTERNAL));
          if (result.is_error()) {
            LOGF(ERROR, "Failed to find driver host request for '%s': %s", child_moniker.c_str(),
                 result.status_string());
          }
        }
      };
  realm_
      ->CreateChild(
          fdecl::wire::CollectionRef{
              .name = "driver-hosts",
          },
          child_decl, child_args_builder.Build())
      .Then(std::move(create_callback));
}

void DriverHostRunner::Start(StartRequestView request, StartCompleter::Sync& completer) {
  std::string url = std::string(request->start_info.resolved_url().get());

  // When we start a driver host, we associate an unforgeable token (the KOID of a
  // zx::event) with the start request, through the use of the numbered_handles
  // field. We do this so:
  //  1. We can securely validate the origin of the request
  //  2. We avoid collisions that can occur when relying on the package URL
  //  3. We avoid relying on the resolved URL matching the package URL
  if (!request->start_info.has_numbered_handles()) {
    LOGF(ERROR, "Failed to start driver host'%s', invalid request", url.c_str());
    completer.Close(ZX_ERR_INVALID_ARGS);
    return;
  }
  auto& handles = request->start_info.numbered_handles();
  if (handles.count() != 1 || !handles[0].handle || handles[0].id != kTokenId) {
    LOGF(ERROR, "Failed to start driver host '%s', invalid request", url.c_str());
    completer.Close(ZX_ERR_INVALID_ARGS);
    return;
  }

  zx::result koid = GetKoid(zx::unowned_handle(handles[0].handle.get()));
  if (koid.is_error()) {
    completer.Close(ZX_ERR_INVALID_ARGS);
    return;
  }

  zx::result result = CallCallback(koid.value(), zx::ok(StartedComponent{
                                                     .info = fidl::ToNatural(request->start_info),
                                                     .controller = std::move(request->controller),
                                                 }));
  if (result.is_error()) {
    LOGF(ERROR, "Failed to start driver host '%s', unknown request", url.c_str());
    completer.Close(ZX_ERR_UNAVAILABLE);
  }
}

zx::result<> DriverHostRunner::CallCallback(zx_koid_t koid,
                                            zx::result<StartedComponent> component) {
  auto it = start_requests_.find(koid);
  if (it == start_requests_.end()) {
    return zx::error(ZX_ERR_NOT_FOUND);
  }
  auto callback = std::move(it->second);
  start_requests_.erase(koid);

  callback(std::move(component));
  return zx::ok();
}

}  // namespace driver_manager
