// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_manager/coordinator.h"

#include <ctype.h>
#include <errno.h>
#include <fidl/fuchsia.io/cpp/wire.h>
#include <fidl/fuchsia.pkg/cpp/wire.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async/cpp/receiver.h>
#include <lib/async/cpp/task.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/ddk/driver.h>
#include <lib/fdio/directory.h>
#include <lib/fdio/io.h>
#include <lib/fidl/coding.h>
#include <lib/fidl/cpp/wire/arena.h>
#include <lib/fidl/cpp/wire/wire_messaging.h>
#include <lib/fit/defer.h>
#include <lib/fzl/owned-vmo-mapper.h>
#include <lib/zircon-internal/ktrace.h>
#include <lib/zx/clock.h>
#include <lib/zx/job.h>
#include <lib/zx/time.h>
#include <limits.h>
#include <stdarg.h>
#include <stdio.h>
#include <string.h>
#include <threads.h>
#include <zircon/assert.h>
#include <zircon/errors.h>
#include <zircon/processargs.h>
#include <zircon/status.h>
#include <zircon/syscalls.h>
#include <zircon/syscalls/policy.h>
#include <zircon/syscalls/system.h>

#include <array>
#include <cstdint>
#include <memory>
#include <string_view>
#include <utility>
#include <vector>

#include <fbl/string_printf.h>
#include <inspector/inspector.h>
#include <src/bringup/lib/mexec/mexec.h>
#include <src/lib/fsl/vmo/sized_vmo.h>
#include <src/lib/fsl/vmo/vector.h>

#include "lib/fidl/cpp/wire/channel.h"
#include "src/devices/bin/driver_manager/driver_development/info_iterator.h"
#include "src/devices/bin/driver_manager/package_resolver.h"
#include "src/devices/bin/driver_manager/v1/composite_node_spec_v1.h"
#include "src/devices/bin/driver_manager/v1/driver_development.h"
#include "src/devices/lib/log/log.h"

namespace {

namespace fdd = fuchsia_driver_development;
namespace fdm = fuchsia_device_manager;

constexpr char kDriverHostName[] = "driver_host";
constexpr char kDriverHostPath[] = "/pkg/bin/driver_host";
constexpr const char* kItemsPath = fidl::DiscoverableProtocolDefaultPath<fuchsia_boot::Items>;

// The driver_host doesn't just define its own __asan_default_options()
// function because that conflicts with the build-system feature of injecting
// such a function based on the `asan_default_options` GN build argument.
// Since driver_host is only ever launched here, it can always get its
// necessary options through its environment variables.  The sanitizer
// runtime combines the __asan_default_options() and environment settings.
constexpr char kAsanEnvironment[] =
    "ASAN_OPTIONS="

    // All drivers have a pure C ABI.  But each individual driver might
    // statically link in its own copy of some C++ library code.  Since no
    // C++ language relationships leak through the driver ABI, each driver is
    // its own whole program from the perspective of the C++ language rules.
    // But the ASan runtime doesn't understand this and wants to diagnose ODR
    // violations when the same global is defined in multiple drivers, which
    // is likely with C++ library use.  There is no real way to teach the
    // ASan instrumentation or runtime about symbol visibility and isolated
    // worlds within the program, so the only thing to do is suppress the ODR
    // violation detection.  This unfortunately means real ODR violations
    // within a single C++ driver won't be caught either.
    "detect_odr_violation=0";

// Currently we check if DriverManager is built using ASAN.
// If it is, then we assume DriverHost is also ASAN.
//
// We currently assume that either the whole system is ASAN or the whole
// system is non-ASAN. One day we might be able to be more flexible about
// which drivers must get loaded into the same driver_host and thus be able
// to use both ASan and non-ASan driver_hosts at the same time when only
// a subset of drivers use ASan.
bool driver_host_is_asan() {
  bool is_asan = false;
#if defined(__has_feature)
#if __has_feature(address_sanitizer)
  is_asan = true;
#endif
#endif
  return is_asan;
}

zx::result<zx::vmo> DriverToVmo(const Driver& driver) {
  ZX_ASSERT(driver.dso_vmo != ZX_HANDLE_INVALID);

  // Return a duplicate of the cached vmo.
  zx::vmo vmo;
  zx_rights_t rights =
      ZX_RIGHTS_BASIC | ZX_RIGHTS_PROPERTY | ZX_RIGHT_READ | ZX_RIGHT_EXECUTE | ZX_RIGHT_MAP;
  if (zx_status_t status = driver.dso_vmo.duplicate(rights, &vmo); status != ZX_OK) {
    LOGF(ERROR, "Failed to duplicate driver vmo %s", zx_status_get_string(status));
    return zx::error(status);
  }
  return zx::ok(std::move(vmo));
}

zx::result<zx::vmo> UrlToVmo(DriverLoader& driver_loader, const std::string& url) {
  const Driver* driver = driver_loader.LoadDriverUrl(url);
  if (!driver) {
    LOGF(ERROR, "Cannot find driver '%s'", url.data());
    return zx::error(ZX_ERR_NOT_FOUND);
  }

  return DriverToVmo(*driver);
}

// Create a stub device in a driver host.
zx_status_t CreateStubDevice(const fbl::RefPtr<Device>& dev,
                             fidl::ServerEnd<fuchsia_device_manager::DeviceController> controller,
                             fbl::RefPtr<DriverHost>& dh) {
  auto coordinator_endpoints = fidl::CreateEndpoints<fdm::Coordinator>();
  if (coordinator_endpoints.is_error()) {
    return coordinator_endpoints.error_value();
  }

  fidl::Arena arena;
  fdm::wire::StubDevice stub{dev->protocol_id()};
  auto type = fdm::wire::DeviceType::WithStub(stub);
  dh->controller()
      ->CreateDevice(std::move(coordinator_endpoints->client), std::move(controller),
                     std::move(type), dev->local_id())
      .ThenExactlyOnce(
          [](fidl::WireUnownedResult<fdm::DriverHostController::CreateDevice>& result) {
            if (!result.ok()) {
              LOGF(ERROR, "Failed to create device: %s",
                   result.error().FormatDescription().c_str());
              return;
            }
            if (result.value().status != ZX_OK) {
              LOGF(ERROR, "Failed to create device: %s",
                   zx_status_get_string(result.value().status));
            }
          });

  dev->Serve(std::move(coordinator_endpoints->server));
  return ZX_OK;
}

// send message to driver_host, requesting the creation of a device
zx_status_t CreateProxyDevice(const fbl::RefPtr<Device>& dev, fbl::RefPtr<DriverHost>& dh,
                              fidl::ServerEnd<fuchsia_device_manager::DeviceController> controller,
                              zx::channel rpc_proxy) {
  // If we don't have a driver name, then create a stub instead.
  if (dev->parent_driver_url().empty()) {
    return CreateStubDevice(dev, std::move(controller), dh);
  }

  auto coordinator_endpoints = fidl::CreateEndpoints<fdm::Coordinator>();
  if (coordinator_endpoints.is_error()) {
    return coordinator_endpoints.error_value();
  }

  fidl::Arena arena;
  zx::result vmo_result = UrlToVmo(dev->coordinator->driver_loader(), dev->parent_driver_url());
  if (vmo_result.is_error()) {
    LOGF(ERROR, "Failed to get VMO for url '%s' %s", dev->parent_driver_url().c_str(),
         vmo_result.status_string());
    return vmo_result.error_value();
  }
  zx::vmo vmo = std::move(vmo_result.value());

  fdm::wire::ProxyDevice proxy{fidl::StringView::FromExternal(dev->parent_driver_url()),
                               std::move(vmo), std::move(rpc_proxy)};
  auto type = fdm::wire::DeviceType::WithProxy(arena, std::move(proxy));

  dh->controller()
      ->CreateDevice(std::move(coordinator_endpoints->client), std::move(controller),
                     std::move(type), dev->local_id())
      .ThenExactlyOnce(
          [](fidl::WireUnownedResult<fdm::DriverHostController::CreateDevice>& result) {
            if (!result.ok()) {
              LOGF(ERROR, "Failed to create device: %s",
                   result.error().FormatDescription().c_str());
              return;
            }
            if (result.value().status != ZX_OK) {
              LOGF(ERROR, "Failed to create device: %s",
                   zx_status_get_string(result.value().status));
            }
          });

  dev->Serve(std::move(coordinator_endpoints->server));
  return ZX_OK;
}

zx_status_t CreateFidlProxyDevice(
    const fbl::RefPtr<Device>& dev, fbl::RefPtr<DriverHost>& dh,
    fidl::ServerEnd<fuchsia_device_manager::DeviceController> controller,
    fidl::ClientEnd<fio::Directory> incoming_dir) {
  auto coordinator_endpoints = fidl::CreateEndpoints<fdm::Coordinator>();
  if (coordinator_endpoints.is_error()) {
    return coordinator_endpoints.error_value();
  }

  fdm::wire::FidlProxyDevice fidl_proxy{std::move(incoming_dir)};
  auto type = fdm::wire::DeviceType::WithFidlProxy(std::move(fidl_proxy));

  dh->controller()
      ->CreateDevice(std::move(coordinator_endpoints->client), std::move(controller),
                     std::move(type), dev->local_id())
      .ThenExactlyOnce(
          [](fidl::WireUnownedResult<fdm::DriverHostController::CreateDevice>& result) {
            if (!result.ok()) {
              LOGF(ERROR, "Failed to create device: %s",
                   result.error().FormatDescription().c_str());
              return;
            }
            if (result.value().status != ZX_OK) {
              LOGF(ERROR, "Failed to create device: %s",
                   zx_status_get_string(result.value().status));
            }
          });

  dev->Serve(std::move(coordinator_endpoints->server));
  return ZX_OK;
}

// Binds the driver to the device by sending a request to driver_host.
zx_status_t BindDriverToDevice(const fbl::RefPtr<Device>& dev, const Driver& driver) {
  auto vmo = DriverToVmo(driver);
  if (vmo.is_error()) {
    return vmo.error_value();
  }

  dev->set_bound_driver(&driver);
  dev->device_controller()
      ->BindDriver(fidl::StringView::FromExternal(driver.url.c_str()), std::move(*vmo),
                   fidl::StringView::FromExternal(driver.default_dispatcher_scheduler_role.c_str()))
      .ThenExactlyOnce([dev](fidl::WireUnownedResult<fdm::DeviceController::BindDriver>& result) {
        if (result.is_peer_closed()) {
          // TODO(fxbug.dev/56208): If we're closed from the driver host we only log a warning,
          // otherwise tests could flake.
          LOGF(WARNING, "Failed to bind driver to device '%s': %s", dev->name().data(),
               result.status_string());
          dev->clear_bound_driver();
          return;
        }
        if (!result.ok()) {
          LOGF(ERROR, "Failed to bind driver to device '%s': %s", dev->name().data(),
               result.status_string());
          dev->clear_bound_driver();
          return;
        }
        if (result.value().status != ZX_OK) {
          LOGF(ERROR, "Failed to bind driver to device '%s': %s", dev->name().data(),
               zx_status_get_string(result.value().status));
          dev->clear_bound_driver();
          return;
        }
      });
  return ZX_OK;
}

}  // namespace

Coordinator::Coordinator(CoordinatorConfig config, InspectManager* inspect_manager,
                         async_dispatcher_t* dispatcher, async_dispatcher_t* firmware_dispatcher)
    : config_(std::move(config)),
      dispatcher_(dispatcher),
      base_resolver_(config_.boot_args),
      inspect_manager_(inspect_manager),
      devfs_(root_devnode_,
             [this]() {
               zx::result diagnostics_client = inspect_manager_->Connect();
               ZX_ASSERT_MSG(diagnostics_client.is_ok(), "%s", diagnostics_client.status_string());
               return std::move(diagnostics_client.value());
             }()),
      package_resolver_(config_.boot_args),
      driver_loader_(config_.boot_args, std::move(config_.driver_index), &base_resolver_,
                     dispatcher, config_.delay_fallback_until_base_drivers_indexed,
                     &package_resolver_),
      firmware_loader_(firmware_dispatcher, config_.path_prefix) {
  bind_driver_manager_ = std::make_unique<BindDriverManager>(this);

  device_manager_ = std::make_unique<DeviceManager>(this, config_.crash_policy);

  composite_node_spec_manager_ = std::make_unique<CompositeNodeSpecManager>(this);

  suspend_resume_manager_ = std::make_unique<SuspendResumeManager>(this, config_.suspend_timeout);
}

Coordinator::~Coordinator() {
  // Device::~Device needs to call into Devfs to complete its cleanup; we must
  // do this ahead of the normal destructor order to avoid reaching into devfs_
  // after it has been dropped.
  root_device_ = nullptr;
}

void Coordinator::LoadV1Drivers(std::string_view root_device_driver) {
  InitCoreDevices(root_device_driver);

  PrepareProxy(root_device_, nullptr);

  // Bind all the drivers we loaded.
  DriverLoader::MatchDeviceConfig config;
  bind_driver_manager_->BindAllDevices(config);

  if (config_.delay_fallback_until_base_drivers_indexed) {
    LOGF(INFO, "Fallback drivers will be loaded after base drivers are indexed.");
  }

  // Schedule the base drivers to load.
  driver_loader_.WaitForBaseDrivers([this]() {
    DriverLoader::MatchDeviceConfig config;
    config.only_return_base_and_fallback_drivers = true;
    bind_driver_manager_->BindAllDevices(config);
  });
}

void Coordinator::InitCoreDevices(std::string_view root_device_driver) {
  root_device_ =
      fbl::MakeRefCounted<Device>(this, "sys", root_device_driver, nullptr, 0, zx::vmo(),
                                  fidl::ClientEnd<fuchsia_device_manager::DeviceController>(),
                                  fidl::ClientEnd<fio::Directory>());
  root_device_->flags = DEV_CTX_IMMORTAL | DEV_CTX_MUST_ISOLATE;

  // Add root_device_ to devfs manually since it's the first device.
  zx_status_t status = root_devnode_->add_child(
      root_device_->name(), ProtocolIdToClassName(root_device_->protocol_id()),
      root_device_->MakeDevfsTarget(), root_device_->devfs);
  ZX_ASSERT_MSG(status == ZX_OK, "Failed to initialize the root device to devfs: %s",
                zx_status_get_string(status));
  root_device_->devfs.publish();
}

zx_status_t Coordinator::NewDriverHost(const char* name, fbl::RefPtr<DriverHost>* out) {
  std::string root_driver_path_arg;
  std::vector<const char*> env;
  if (driver_host_is_asan()) {
    env.push_back(kAsanEnvironment);
  }

  auto driver_host_env = (*boot_args())->Collect("driver.");
  if (!driver_host_env.ok()) {
    return driver_host_env.status();
  }

  std::vector<std::string> strings;
  for (auto& entry : driver_host_env.value().results) {
    strings.emplace_back(entry.data(), entry.size());
  }

  // Make the clock backstop boot arg available to drivers that
  // deal with time (RTC).
  // TODO(fxbug.dev/60668): Remove once UTC time is removed from the kernel.
  auto backstop_env = (*boot_args())->GetString("clock.backstop");
  if (!backstop_env.ok()) {
    return backstop_env.status();
  }

  auto backstop_env_value = std::move(backstop_env.value().value);
  if (!backstop_env_value.is_null()) {
    strings.push_back(std::string("clock.backstop=") +
                      std::string(backstop_env_value.data(), backstop_env_value.size()));
  }

  for (auto& entry : strings) {
    env.push_back(entry.data());
  }

  if (config_.verbose) {
    env.push_back("devmgr.verbose=true");
  }
  root_driver_path_arg = "devmgr.root_driver_path=" + config_.path_prefix + "driver/";
  env.push_back(root_driver_path_arg.c_str());

  env.push_back(nullptr);

  DriverHostConfig config{
      .name = name,
      .binary = kDriverHostPath,
      .env = env.data(),
      .job = zx::unowned_job(config_.driver_host_job),
      .root_resource = zx::unowned_resource(root_resource()),
      .loader_service_connector = &loader_service_connector_,
      .fs_provider = config_.fs_provider,
      .coordinator = this,
  };
  fbl::RefPtr<DriverHost> dh;
  zx_status_t status = DriverHost::Launch(config, &dh);
  if (status != ZX_OK) {
    return status;
  }
  launched_first_driver_host_ = true;

  VLOGF(1, "New driver_host %p", dh.get());
  *out = std::move(dh);
  return ZX_OK;
}

zx_status_t Coordinator::MakeVisible(const fbl::RefPtr<Device>& dev) {
  if (dev->state() == Device::State::kDead) {
    return ZX_ERR_BAD_STATE;
  }
  if (dev->state() == Device::State::kInitializing) {
    // This should only be called in response to the init hook completing.
    return ZX_ERR_BAD_STATE;
  }
  if (dev->flags & DEV_CTX_INVISIBLE) {
    dev->flags &= ~DEV_CTX_INVISIBLE;
    dev->devfs.publish();
    zx_status_t r = dev->SignalReadyForBind();
    if (r != ZX_OK) {
      return r;
    }
  }
  return ZX_OK;
}

// Traverse up the device tree to find the metadata with the matching |type|.
// |buffer| can be nullptr, in which case only the size of the metadata is
// returned. This is used by GetMetadataSize method.
zx_status_t Coordinator::GetMetadata(const fbl::RefPtr<Device>& dev, uint32_t type, void* buffer,
                                     size_t buflen, size_t* size) {
  // search dev and its parent devices for a match
  for (fbl::RefPtr<Device> current = dev; current != nullptr; current = current->parent()) {
    for (const auto& md : current->metadata()) {
      if (md.type == type) {
        if (buffer != nullptr) {
          if (md.length > buflen) {
            return ZX_ERR_BUFFER_TOO_SMALL;
          }
          memcpy(buffer, md.Data(), md.length);
        }
        *size = md.length;
        return ZX_OK;
      }
    }
    // search fragments of composite devices
    if (std::optional composite = current->composite(); composite.has_value()) {
      for (auto& fragment : composite.value().get().fragments()) {
        if (!fragment.IsBound()) {
          continue;
        }
        if (GetMetadata(fragment.bound_device(), type, buffer, buflen, size) == ZX_OK) {
          return ZX_OK;
        }
      }
    }
  }

  return ZX_ERR_NOT_FOUND;
}

zx_status_t Coordinator::AddMetadata(const fbl::RefPtr<Device>& dev, uint32_t type,
                                     const void* data, uint32_t length) {
  std::unique_ptr<Metadata> md;
  zx_status_t status = Metadata::Create(length, &md);
  if (status != ZX_OK) {
    return status;
  }

  md->type = type;
  md->length = length;
  memcpy(md->Data(), data, length);
  dev->AddMetadata(std::move(md));
  return ZX_OK;
}

// Create the proxy node for the given device if it doesn't exist and ensure it
// has a driver_host.  If |target_driver_host| is not nullptr and the proxy doesn't have
// a driver_host yet, |target_driver_host| will be used for it.  Otherwise a new driver_host
// will be created.
zx_status_t Coordinator::PrepareProxy(const fbl::RefPtr<Device>& dev,
                                      fbl::RefPtr<DriverHost> target_driver_host) {
  ZX_ASSERT(!(dev->flags & DEV_CTX_PROXY));
  // If we already have a proxy we don't have to do anything.
  if (dev->proxy() != nullptr) {
    return ZX_OK;
  }

  zx::result controller = fidl::CreateEndpoints<fuchsia_device_manager::DeviceController>();
  if (controller.is_error()) {
    return controller.status_value();
  }
  auto& [controller_client, controller_server] = controller.value();

  const zx_status_t status = dev->CreateProxy(std::move(controller_client));
  if (status != ZX_OK) {
    LOGF(ERROR, "Cannot create proxy device '%s': %s", dev->name().data(),
         zx_status_get_string(status));
    return status;
  }

  // Instantiate the proxy's driver host.
  zx::channel child_channel, parent_channel;
  // the immortal root devices do not provide proxy rpc
  bool need_proxy_rpc = !(dev->flags & DEV_CTX_IMMORTAL);

  if (need_proxy_rpc || dev == root_device_) {
    // create rpc channel for proxy device to talk to the busdev it proxies
    const zx_status_t status = zx::channel::create(0, &child_channel, &parent_channel);
    if (status != ZX_OK) {
      return status;
    }
  }
  if (target_driver_host == nullptr) {
    const zx_status_t status = NewDriverHost(kDriverHostName, &target_driver_host);
    if (status != ZX_OK) {
      LOGF(ERROR, "Failed to create driver_host '%s': %s", kDriverHostName,
           zx_status_get_string(status));
      return status;
    }
  }

  dev->proxy()->set_host(std::move(target_driver_host));
  if (const zx_status_t status =
          CreateProxyDevice(dev->proxy(), dev->proxy()->host(), std::move(controller_server),
                            std::move(child_channel));
      status != ZX_OK) {
    LOGF(ERROR, "Failed to create proxy device '%s' in driver_host '%s': %s", dev->name().data(),
         kDriverHostName, zx_status_get_string(status));
    return status;
  }
  if (need_proxy_rpc) {
    if (const fidl::Status result =
            dev->device_controller()->ConnectProxy(std::move(parent_channel));
        !result.ok()) {
      LOGF(ERROR, "Failed to connect to proxy device '%s' in driver_host '%s': %s",
           dev->name().data(), kDriverHostName, result.status_string());
    }
  }
  if (dev == root_device_) {
    if (const zx_status_t status = fdio_service_connect(kItemsPath, parent_channel.release());
        status != ZX_OK) {
      LOGF(ERROR, "Failed to connect to %s: %s", kItemsPath, zx_status_get_string(status));
    }
  }

  return ZX_OK;
}

zx_status_t Coordinator::PrepareFidlProxy(const fbl::RefPtr<Device>& dev,
                                          fbl::RefPtr<DriverHost> target_driver_host,
                                          fbl::RefPtr<Device>* fidl_proxy_out) {
  zx::result controller = fidl::CreateEndpoints<fuchsia_device_manager::DeviceController>();
  if (controller.is_error()) {
    return controller.status_value();
  }
  auto& [controller_client, controller_server] = controller.value();

  if (zx_status_t status = dev->CreateFidlProxy(std::move(controller_client), fidl_proxy_out);
      status != ZX_OK) {
    LOGF(ERROR, "Failed to create FIDL proxy device '%s': %s", dev->name().c_str(),
         zx_status_get_string(status));
    return status;
  }

  if (target_driver_host == nullptr) {
    if (zx_status_t status = NewDriverHost(kDriverHostName, &target_driver_host); status != ZX_OK) {
      LOGF(ERROR, "Failed to create driver_host '%s': %s", kDriverHostName,
           zx_status_get_string(status));
      return status;
    }
  }
  (*fidl_proxy_out)->set_host(std::move(target_driver_host));
  fidl::ClientEnd<fio::Directory> outgoing_dir;
  if (dev->has_outgoing_directory()) {
    zx::result clone = dev->clone_outgoing_dir();
    if (clone.is_error()) {
      LOGF(ERROR, "Failed to clone device outgoing directory '%s': %s", dev->name().c_str(),
           clone.status_string());
      return clone.status_value();
    }
    outgoing_dir = std::move(clone.value());
  }
  if (zx_status_t status =
          CreateFidlProxyDevice(*fidl_proxy_out, (*fidl_proxy_out)->host(),
                                std::move(controller_server), std::move(outgoing_dir));
      status != ZX_OK) {
    LOGF(ERROR, "Failed to create proxy device '%s' in driver_host '%s': %s", dev->name().c_str(),
         kDriverHostName, zx_status_get_string(status));
    return status;
  }

  return ZX_OK;
}

zx_status_t Coordinator::AttemptBind(const MatchedDriverInfo matched_driver,
                                     const fbl::RefPtr<Device>& dev) {
  // Cannot bind driver to an already bound device.
  if (dev->IsAlreadyBound()) {
    return ZX_ERR_ALREADY_BOUND;
  }

  if (matched_driver.is_dfv2) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  const Driver* driver_ptr = driver_loader_.LoadDriverUrl(matched_driver.component_url);
  if (!driver_ptr) {
    LOGF(ERROR, "Failed to load %s", matched_driver.component_url.c_str());
    return ZX_ERR_INTERNAL;
  }
  const Driver& driver = *driver_ptr;

  if (!driver_host_is_asan() && driver.is_asan) {
    LOGF(ERROR, "%s (%s) requires ASAN, but we are not in an ASAN environment", driver.url.data(),
         driver.name.data());
    return ZX_ERR_BAD_STATE;
  }

  // A driver is colocated with its parent device in three circumstances:
  //
  //   (1) The parent device is a composite device. Colocation for composites is handled when the
  //       composite is created, and the child of the composite is always colocated. Note that the
  //       other devices that the composite comprises may still be in separate hosts from the child
  //       driver.
  //   (2) The driver being bound is the fragment driver, which is always colocated.
  //   (3) The driver specified `colocate = true` in its component manifest, AND the parent device
  //       did not enforce isolation with the MUST_ISOLATE flag.
  if (dev->is_composite() || std::string_view{driver.url.c_str()} == fdf::kFragmentDriverUrl ||
      (matched_driver.colocate && !(dev->flags & DEV_CTX_MUST_ISOLATE))) {
    VLOGF(1, "Binding driver to %s in same driver host as parent", dev->name().data());
    // non-busdev is pretty simple
    if (dev->host() == nullptr) {
      LOGF(ERROR, "Cannot bind to device '%s', it has no driver_host", dev->name().data());
      return ZX_ERR_BAD_STATE;
    }
    return BindDriverToDevice(dev, driver);
  }

  // If we've gotten this far, we need to prepare a proxy because the driver is going to be bound in
  // a different host than its parent device. The proxy can either be a FIDL proxy, which is a
  // lightweight proxy that exposes the parent's outgoing directory so that the child can connect to
  // its FIDL protocol, or a regular proxy, which is a much more complicated device that implements
  // Banjo proxying.
  //
  // We should prepare a FIDL proxy if the driver has set `colocate = false`, because we only set
  // that flag if the driver is going to be using FIDL.
  zx_status_t status;
  if (!matched_driver.colocate) {
    VLOGF(1, "Preparing FIDL proxy for %s", dev->name().data());
    fbl::RefPtr<Device> fidl_proxy;
    status = PrepareFidlProxy(dev, nullptr /* target_driver_host */, &fidl_proxy);
    if (status != ZX_OK) {
      return status;
    }
    status = BindDriverToDevice(fidl_proxy, driver);
  } else {
    VLOGF(1, "Preparing Banjo proxy for %s", dev->name().data());
    status = PrepareProxy(dev, nullptr /* target_driver_host */);
    if (status != ZX_OK) {
      return status;
    }
    status = BindDriverToDevice(dev->proxy(), driver);
  }
  // TODO(swetland): arrange to mark us unbound when the proxy (or its driver_host) goes away
  if ((status == ZX_OK) && !(dev->flags & DEV_CTX_ALLOW_MULTI_COMPOSITE)) {
    dev->flags |= DEV_CTX_BOUND;
  }
  return status;
}

fuchsia_device_manager::SystemPowerState Coordinator::shutdown_system_state() const {
  zx::result client = component::Connect<fuchsia_device_manager::SystemStateTransition>();
  if (client.is_error()) {
    LOGF(ERROR, "Failed to connect to StateStateTransition: %s", client.status_string());
    return config_.default_shutdown_system_state;
  }

  fidl::Result result = fidl::Call(*client)->GetTerminationSystemState();
  if (result.is_error()) {
    LOGF(ERROR, "Failed to get termination system state: %s",
         result.error_value().FormatDescription().c_str());
    return config_.default_shutdown_system_state;
  }

  return result->state();
}

zx_status_t Coordinator::AddCompositeNodeSpec(
    const fbl::RefPtr<Device>& dev, std::string_view name,
    fuchsia_device_manager::wire::CompositeNodeSpecDescriptor spec) {
  if (spec.parents.count() == 0) {
    return ZX_ERR_INVALID_ARGS;
  }

  auto spec_result = composite_node_specs::CompositeNodeSpecV1::Create(
      CompositeNodeSpecCreateInfo{
          .name = std::string(name.data()),
          .size = spec.parents.count(),
      },
      spec, *device_manager_);
  if (!spec_result.is_ok()) {
    LOGF(ERROR, "Failed to create composite node spec");
    return spec_result.status_value();
  }

  fidl::Arena allocator;
  auto fidl_spec = fdf::wire::CompositeNodeSpec::Builder(allocator)
                       .name(fidl::StringView(allocator, name))
                       .parents(std::move(spec.parents))
                       .Build();

  auto result = composite_node_spec_manager_->AddSpec(fidl_spec, std::move(spec_result.value()));
  if (!result.is_ok()) {
    LOGF(ERROR, "Failed to add spec to the CompositeNodeSpecManager: %d.", result.error_value());
    return ZX_ERR_INVALID_ARGS;
  }

  return ZX_OK;
}

void Coordinator::GetDriverInfo(GetDriverInfoRequestView request,
                                GetDriverInfoCompleter::Sync& completer) {
  auto driver_index_client = component::Connect<fuchsia_driver_development::DriverIndex>();
  if (driver_index_client.is_error()) {
    LOGF(WARNING, "Failed to connect to fuchsia_driver_development::DriverIndex");
    return;
  }

  fidl::WireSyncClient driver_index{std::move(*driver_index_client)};
  auto info_result =
      driver_index->GetDriverInfo(request->driver_filter, std::move(request->iterator));

  // There are still some environments where we can't connect to DriverIndex.
  if (info_result.status() != ZX_OK) {
    LOGF(INFO, "DriverIndex:GetDriverInfo failed: %d", info_result.status());
  }
}

void Coordinator::GetCompositeNodeSpecs(GetCompositeNodeSpecsRequestView request,
                                        GetCompositeNodeSpecsCompleter::Sync& completer) {
  auto driver_index_client = component::Connect<fuchsia_driver_development::DriverIndex>();
  if (driver_index_client.is_error()) {
    LOGF(WARNING, "Failed to connect to fuchsia_driver_development::DriverIndex");
    return;
  }

  fidl::WireSyncClient driver_index{std::move(*driver_index_client)};
  auto info_result =
      driver_index->GetCompositeNodeSpecs(request->name_filter, std::move(request->iterator));

  // There are still some environments where we can't connect to DriverIndex.
  if (!info_result.ok()) {
    LOGF(INFO, "DriverIndex:GetCompositeNodeSpecs failed: %d", info_result.status());
  }
}

void Coordinator::DisableMatchWithDriverUrl(DisableMatchWithDriverUrlRequestView request,
                                            DisableMatchWithDriverUrlCompleter::Sync& completer) {
  auto driver_index_client = component::Connect<fuchsia_driver_development::DriverIndex>();
  if (driver_index_client.is_error()) {
    LOGF(ERROR, "Failed to connect to service '%s': %s",
         fidl::DiscoverableProtocolName<fuchsia_driver_development::DriverIndex>,
         driver_index_client.status_string());
    completer.Close(driver_index_client.status_value());
    return;
  }

  fidl::WireSyncClient driver_index{std::move(*driver_index_client)};
  auto disable_result = driver_index->DisableMatchWithDriverUrl(request->driver_url);
  if (!disable_result.ok()) {
    LOGF(ERROR, "Failed to call DriverIndex::DisableMatchWithDriverUrl: %s\n",
         disable_result.error().FormatDescription().c_str());
    completer.Close(disable_result.error().status());
    return;
  }

  completer.Reply();
}

void Coordinator::ReEnableMatchWithDriverUrl(ReEnableMatchWithDriverUrlRequestView request,
                                             ReEnableMatchWithDriverUrlCompleter::Sync& completer) {
  auto driver_index_client = component::Connect<fuchsia_driver_development::DriverIndex>();
  if (driver_index_client.is_error()) {
    LOGF(ERROR, "Failed to connect to service '%s': %s",
         fidl::DiscoverableProtocolName<fuchsia_driver_development::DriverIndex>,
         driver_index_client.status_string());
    completer.Close(driver_index_client.status_value());
    return;
  }

  fidl::WireSyncClient driver_index{std::move(*driver_index_client)};
  auto un_disable_result = driver_index->ReEnableMatchWithDriverUrl(request->driver_url);
  if (!un_disable_result.ok()) {
    LOGF(ERROR, "Failed to call DriverIndex::ReEnableMatchWithDriverUrl: %s\n",
         un_disable_result.error().FormatDescription().c_str());
    completer.Close(un_disable_result.error().status());
    return;
  }

  completer.Reply(un_disable_result.value());
}

void Coordinator::GetDeviceInfo(GetDeviceInfoRequestView request,
                                GetDeviceInfoCompleter::Sync& completer) {
  std::vector<fbl::RefPtr<const Device>> device_list;
  for (auto& device : device_manager_->devices()) {
    if (device.is_fragment_proxy_device()) {
      continue;
    }

    if (request->device_filter.empty()) {
      device_list.emplace_back(&device);
    } else {
      for (const fidl::StringView& device_filter : request->device_filter) {
        std::string path = device.MakeTopologicalPath();
        bool matches = false;
        // TODO(fxbug.dev/115717): Matches should also check the /dev/class/ path.
        if (request->exact_match) {
          matches = path == device_filter.get();
        } else {
          matches = path.find(device_filter.get()) != std::string_view::npos;
        }
        if (matches) {
          device_list.emplace_back(&device);
        }
      }
    }
  }

  auto arena = std::make_unique<fidl::Arena<512>>();
  auto result = ::GetDeviceInfo(*arena, device_list);
  if (result.is_error()) {
    request->iterator.Close(result.status_value());
    return;
  }

  auto iterator = std::make_unique<driver_development::DeviceInfoIterator>(std::move(arena),
                                                                           std::move(*result));
  fidl::BindServer(dispatcher(), std::move(request->iterator), std::move(iterator),
                   [](auto* server, fidl::UnbindInfo info, auto channel) {
                     if (!info.is_peer_closed()) {
                       LOGF(WARNING, "Closed DeviceInfoIterator: %s", info.lossy_description());
                     }
                   });
}

void Coordinator::GetCompositeInfo(GetCompositeInfoRequestView request,
                                   GetCompositeInfoCompleter::Sync& completer) {
  auto arena = std::make_unique<fidl::Arena<512>>();
  auto list = device_manager_->GetLegacyCompositeInfoList(*arena);

  auto spec_composite_list = composite_node_spec_manager_->GetCompositeInfo(*arena);
  list.reserve(list.size() + spec_composite_list.size());
  list.insert(list.end(), std::make_move_iterator(spec_composite_list.begin()),
              std::make_move_iterator(spec_composite_list.end()));

  auto iterator = std::make_unique<driver_development::CompositeInfoIterator>(std::move(arena),
                                                                              std::move(list));
  fidl::BindServer(dispatcher(), std::move(request->iterator), std::move(iterator),
                   [](auto* server, fidl::UnbindInfo info, auto channel) {
                     if (!info.is_peer_closed()) {
                       LOGF(WARNING, "Closed CompositeInfoIterator: %s", info.lossy_description());
                     }
                   });
}

void Coordinator::BindAllUnboundNodes(BindAllUnboundNodesCompleter::Sync& completer) {
  LOGF(WARNING, "BindAllUnboundNodes is only supported in DFv2.");
  completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
}

void Coordinator::IsDfv2(IsDfv2Completer::Sync& completer) { completer.Reply(false); }

void Coordinator::AddTestNode(AddTestNodeRequestView request,
                              AddTestNodeCompleter::Sync& completer) {
  LOGF(WARNING, "AddTestNode is only supported in DFv2.");
  completer.ReplyError(fdf::wire::NodeError::kInternal);
}

void Coordinator::RemoveTestNode(RemoveTestNodeRequestView request,
                                 RemoveTestNodeCompleter::Sync& completer) {
  LOGF(WARNING, "RemoveTestNode is only supported in DFv2.");
  completer.ReplyError(ZX_ERR_NOT_SUPPORTED);
}

void Coordinator::ShutdownAllDrivers(fit::callback<void()> callback) {
  suspend_resume_manager().Suspend(
      suspend_resume_manager().GetSuspendFlagsFromSystemPowerState(shutdown_system_state()),
      [cb = std::move(callback)](zx_status_t status) mutable {
        if (status != ZX_OK) {
          // TODO(https://fxbug.dev/56208): Change this log back to error once isolated devmgr
          // is fixed.
          LOGF(WARNING, "Error suspending devices while stopping the component:%s",
               zx_status_get_string(status));
        }
        LOGF(INFO, "Coordinator finished shutting down drivers");
        cb();
      });
}

void Coordinator::ShutdownPkgDrivers(fit::callback<void()> callback) {
  suspend_resume_manager().suspend_handler().UnregisterSystemStorageForShutdown(
      [cb = std::move(callback)](zx_status_t status) mutable { cb(); });
}

void Coordinator::PublishDriverDevelopmentService(component::OutgoingDirectory& outgoing) {
  auto driver_dev = [this](fidl::ServerEnd<fdd::DriverDevelopment> request) {
    fidl::BindServer(dispatcher_, std::move(request), this,
                     [](Coordinator* self, fidl::UnbindInfo info,
                        fidl::ServerEnd<fdd::DriverDevelopment> server_end) {
                       if (info.is_user_initiated()) {
                         return;
                       }
                       if (info.is_peer_closed()) {
                         // For this development protocol, the client is free to disconnect
                         // at any time.
                         return;
                       }
                       LOGF(ERROR, "Error serving '%s': %s",
                            fidl::DiscoverableProtocolName<fdd::DriverDevelopment>,
                            info.FormatDescription().c_str());
                     });
  };
  auto result = outgoing.AddUnmanagedProtocol<fdd::DriverDevelopment>(driver_dev);
  ZX_ASSERT(result.is_ok());
}

void Coordinator::InitOutgoingServices(component::OutgoingDirectory& outgoing) {
  outgoing_ = &outgoing;
}

// TODO(fxb/107737): Ideally, we try to match and bind all devices, regardless if they
// match with a composite node spec or not. However, this causes issues because the driver manager
// currently can't catch when a device is in the process of its Bind() function. As such,
// this can create an infinite loop of the same device calling its Bind() nonstop. As a
// short-term solution, we can following how Composite Devices just try to match and
// bind devices to its fragments.
void Coordinator::BindNodesForCompositeNodeSpec() {
  for (auto& dev : device_manager_->devices()) {
    auto status = bind_driver_manager_->MatchAndBindCompositeNodeSpec(fbl::RefPtr(&dev));
    if (status != ZX_OK) {
      // LOGF(WARNING, "Failed to bind device '%s': %d", dev.name().data(), status);
    }
  }
}

void Coordinator::AddSpecToDriverIndex(fuchsia_driver_framework::wire::CompositeNodeSpec spec,
                                       AddToIndexCallback callback) {
  driver_loader_.AddCompositeNodeSpec(spec, std::move(callback));
}

void Coordinator::RestartDriverHosts(RestartDriverHostsRequestView request,
                                     RestartDriverHostsCompleter::Sync& completer) {
  std::string_view driver_path(request->driver_path.data(), request->driver_path.size());

  // Find devices containing the driver.
  uint32_t count = 0;
  for (auto& dev : device_manager_->devices()) {
    // Call remove on the device's driver host if it contains the driver.
    if (dev.parent_driver_url().compare(driver_path) == 0) {
      LOGF(INFO, "Device %s found in restart driver hosts.", dev.name().data());
      LOGF(INFO, "Shutting down host: %ld.", dev.host()->koid());

      // Unbind and Remove all the devices in the Driver Host.
      device_manager_->ScheduleUnbindRemoveAllDevices(dev.host());
      count++;
    }
  }

  completer.ReplySuccess(count);
}
