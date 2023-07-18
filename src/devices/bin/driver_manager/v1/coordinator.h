// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BIN_DRIVER_MANAGER_V1_COORDINATOR_H_
#define SRC_DEVICES_BIN_DRIVER_MANAGER_V1_COORDINATOR_H_

#include <fidl/fuchsia.boot/cpp/wire.h>
#include <fidl/fuchsia.device.manager/cpp/fidl.h>
#include <fidl/fuchsia.driver.development/cpp/wire.h>
#include <fidl/fuchsia.driver.framework/cpp/wire.h>
#include <fidl/fuchsia.driver.index/cpp/wire.h>
#include <fidl/fuchsia.fshost/cpp/wire.h>
#include <lib/async/cpp/wait.h>
#include <lib/async/dispatcher.h>
#include <lib/component/outgoing/cpp/outgoing_directory.h>
#include <lib/ddk/binding.h>
#include <lib/ddk/device.h>
#include <lib/fidl/cpp/wire/server.h>
#include <lib/stdcompat/optional.h>
#include <lib/zircon-internal/thread_annotations.h>
#include <lib/zx/channel.h>
#include <lib/zx/event.h>
#include <lib/zx/job.h>
#include <lib/zx/process.h>
#include <lib/zx/result.h>
#include <lib/zx/vmo.h>
#include <zircon/types.h>

#include <memory>
#include <string_view>
#include <utility>

#include <fbl/auto_lock.h>
#include <fbl/intrusive_double_list.h>
#include <fbl/string.h>
#include <fbl/vector.h>

#include "src/devices/bin/driver_manager/composite_node_spec/composite_node_spec_manager.h"
#include "src/devices/bin/driver_manager/devfs/devfs.h"
#include "src/devices/bin/driver_manager/inspect.h"
#include "src/devices/bin/driver_manager/v1/bind_driver_manager.h"
#include "src/devices/bin/driver_manager/v1/device.h"
#include "src/devices/bin/driver_manager/v1/device_manager.h"
#include "src/devices/bin/driver_manager/v1/driver.h"
#include "src/devices/bin/driver_manager/v1/driver_host.h"
#include "src/devices/bin/driver_manager/v1/driver_loader.h"
#include "src/devices/bin/driver_manager/v1/firmware_loader.h"
#include "src/devices/bin/driver_manager/v1/package_resolver.h"
#include "src/devices/bin/driver_manager/v1/suspend_resume_manager.h"
#include "src/devices/bin/driver_manager/v2/node_remover.h"

namespace fdf {
using namespace fuchsia_driver_framework;
}  // namespace fdf
namespace fdi = fuchsia_driver_index;

class BindDriverManager;
class DeviceManager;
class DriverHostLoaderService;
class FsProvider;
class SuspendResumeManager;
class SystemStateManager;

constexpr zx::duration kDefaultResumeTimeout = zx::sec(30);
constexpr zx::duration kDefaultSuspendTimeout = zx::sec(30);

using ResumeCallback = std::function<void(zx_status_t)>;

// The action to take when we witness a driver host crash.
enum class DriverHostCrashPolicy {
  // Restart the driver host, with exponential backoff, up to 3 times.
  // This will only be triggered if the driver host which host's the driver which created the
  // parent device being bound to doesn't also crash.
  // TODO(fxbug.dev/66442): Handle composite devices better (they don't seem to restart with this
  // policy set).
  kRestartDriverHost,
  // Reboot the system via the power manager.
  kRebootSystem,
  // Don't take any action, other than cleaning up some internal driver manager state.
  kDoNothing,
};

struct CoordinatorConfig {
  // Initial root resource from the kernel.
  zx::resource root_resource;
  // Mexec resource from the kernel.
  zx::resource mexec_resource;
  // Job for all driver_hosts.
  zx::job driver_host_job;
  // Client for the Arguments service.
  fidl::WireSyncClient<fuchsia_boot::Arguments>* boot_args;
  // Client for the DriverIndex.
  fidl::WireSharedClient<fdi::DriverIndex> driver_index;
  fidl::WireClient<fuchsia_component::Realm> realm;
  // Whether we should wait until base drivers are indexed before binding fallback drivers.
  bool delay_fallback_until_base_drivers_indexed = false;
  // Whether to enable verbose logging.
  bool verbose = false;
  // Timeout for system wide suspend
  zx::duration suspend_timeout = kDefaultSuspendTimeout;
  // Timeout for system wide resume
  zx::duration resume_timeout = kDefaultResumeTimeout;
  // System will be transitioned to this system power state during
  // component shutdown.
  fuchsia_device_manager::SystemPowerState default_shutdown_system_state =
      fuchsia_device_manager::SystemPowerState::kReboot;
  // Something to clone a handle from the environment to pass to a Devhost.
  FsProvider* fs_provider = nullptr;
  // The path prefix to find binaries, drivers, etc. Typically this is "/boot/", but in test
  // environments this might be different.
  std::string path_prefix = "/boot/";
  // The decision to make when we encounter a driver host crash.
  DriverHostCrashPolicy crash_policy = DriverHostCrashPolicy::kRestartDriverHost;
};

class Coordinator : public CompositeManagerBridge,
                    public fidl::WireServer<fuchsia_driver_development::DriverDevelopment>,
                    public dfv2::NodeRemover {
 public:
  Coordinator(const Coordinator&) = delete;
  Coordinator& operator=(const Coordinator&) = delete;
  Coordinator(Coordinator&&) = delete;
  Coordinator& operator=(Coordinator&&) = delete;

  Coordinator(CoordinatorConfig config, InspectManager* inspect_manager,
              async_dispatcher_t* dispatcher, async_dispatcher_t* firmware_dispatcher);
  ~Coordinator();

  void InitOutgoingServices(component::OutgoingDirectory& outgoing);
  void PublishDriverDevelopmentService(component::OutgoingDirectory& outgoing);

  // Initialization functions for DFv1. InitCoreDevices() is public for testing only.
  void LoadV1Drivers(std::string_view root_device_driver);
  void InitCoreDevices(std::string_view root_device_driver);

  zx_status_t BindDriverToNodeGroup(const MatchedDriver& driver, const fbl::RefPtr<Device>& dev);

  zx_status_t AddCompositeNodeSpec(const fbl::RefPtr<Device>& dev, std::string_view name,
                                   fuchsia_device_manager::wire::CompositeNodeSpecDescriptor spec);

  zx_status_t MakeVisible(const fbl::RefPtr<Device>& dev);

  zx_status_t GetMetadata(const fbl::RefPtr<Device>& dev, uint32_t type, void* buffer,
                          size_t buflen, size_t* size);
  zx_status_t GetMetadataSize(const fbl::RefPtr<Device>& dev, uint32_t type, size_t* size) {
    return GetMetadata(dev, type, nullptr, 0, size);
  }
  zx_status_t AddMetadata(const fbl::RefPtr<Device>& dev, uint32_t type, const void* data,
                          uint32_t length);

  zx_status_t PrepareProxy(const fbl::RefPtr<Device>& dev,
                           fbl::RefPtr<DriverHost> target_driver_host);
  zx_status_t PrepareFidlProxy(const fbl::RefPtr<Device>& dev,
                               fbl::RefPtr<DriverHost> target_driver_host,
                               fbl::RefPtr<Device>* fidl_proxy_out);

  // Function to attempt binding a driver to the device.
  zx_status_t AttemptBind(const MatchedDriverInfo matched_driver, const fbl::RefPtr<Device>& dev);

  zx_status_t NewDriverHost(const char* name, fbl::RefPtr<DriverHost>* out);

  // These methods are used by the DriverHost class to register in the coordinator's bookkeeping
  void RegisterDriverHost(DriverHost* dh) { driver_hosts_.push_back(dh); }
  void UnregisterDriverHost(DriverHost* dh) { driver_hosts_.erase(*dh); }

  uint32_t GetNextDfv2DeviceId() { return next_dfv2_device_id_++; }

  // Setter functions.
  void set_running(bool running) { running_ = running; }
  void set_loader_service_connector(LoaderServiceConnector loader_service_connector) {
    loader_service_connector_ = std::move(loader_service_connector);
  }

  // Getter functions.
  async_dispatcher_t* dispatcher() const { return dispatcher_; }
  const zx::resource& root_resource() const { return config_.root_resource; }
  const zx::resource& mexec_resource() const { return config_.mexec_resource; }
  zx::duration resume_timeout() const { return config_.resume_timeout; }
  fidl::WireSyncClient<fuchsia_boot::Arguments>* boot_args() const { return config_.boot_args; }
  fuchsia_device_manager::SystemPowerState shutdown_system_state() const;
  const fbl::RefPtr<Device>& root_device() { return root_device_; }
  Devfs& devfs() { return devfs_; }
  SuspendResumeManager& suspend_resume_manager() { return *suspend_resume_manager_; }
  InspectManager& inspect_manager() { return *inspect_manager_; }
  DriverLoader& driver_loader() { return driver_loader_; }
  DeviceManager* device_manager() const { return device_manager_.get(); }
  CompositeNodeSpecManager& composite_node_spec_manager() const {
    return *composite_node_spec_manager_;
  }
  BindDriverManager& bind_driver_manager() const { return *bind_driver_manager_; }
  const FirmwareLoader& firmware_loader() const { return firmware_loader_; }

  component::OutgoingDirectory& outgoing() { return *outgoing_; }
  std::optional<Devnode>& root_devnode() { return root_devnode_; }

 private:
  // CompositeManagerBridge interface
  void BindNodesForCompositeNodeSpec() override;
  void AddSpecToDriverIndex(fuchsia_driver_framework::wire::CompositeNodeSpec spec,
                            AddToIndexCallback callback) override;

  // fuchsia.driver.development/DriverDevelopment interface
  void RestartDriverHosts(RestartDriverHostsRequestView request,
                          RestartDriverHostsCompleter::Sync& completer) override;
  void GetDriverInfo(GetDriverInfoRequestView request,
                     GetDriverInfoCompleter::Sync& completer) override;
  void GetCompositeNodeSpecs(GetCompositeNodeSpecsRequestView request,
                             GetCompositeNodeSpecsCompleter::Sync& completer) override;
  void DisableMatchWithDriverUrl(DisableMatchWithDriverUrlRequestView request,
                                 DisableMatchWithDriverUrlCompleter::Sync& completer) override;
  void ReEnableMatchWithDriverUrl(ReEnableMatchWithDriverUrlRequestView request,
                                  ReEnableMatchWithDriverUrlCompleter::Sync& completer) override;
  void GetDeviceInfo(GetDeviceInfoRequestView request,
                     GetDeviceInfoCompleter::Sync& completer) override;
  void GetCompositeInfo(GetCompositeInfoRequestView request,
                        GetCompositeInfoCompleter::Sync& completer) override;
  void BindAllUnboundNodes(BindAllUnboundNodesCompleter::Sync& completer) override;
  void IsDfv2(IsDfv2Completer::Sync& completer) override;
  void AddTestNode(AddTestNodeRequestView request, AddTestNodeCompleter::Sync& completer) override;
  void RemoveTestNode(RemoveTestNodeRequestView request,
                      RemoveTestNodeCompleter::Sync& completer) override;

  void ShutdownAllDrivers(fit::callback<void()> callback) override;
  void ShutdownPkgDrivers(fit::callback<void()> callback) override;

  CoordinatorConfig config_;
  async_dispatcher_t* const dispatcher_;
  bool running_ = false;
  bool launched_first_driver_host_ = false;
  LoaderServiceConnector loader_service_connector_;

  internal::BasePackageResolver base_resolver_;

  // All DriverHosts
  fbl::DoublyLinkedList<DriverHost*> driver_hosts_;

  InspectManager* const inspect_manager_;

  fbl::RefPtr<Device> root_device_;

  std::optional<Devnode> root_devnode_;
  Devfs devfs_;

  internal::PackageResolver package_resolver_;
  DriverLoader driver_loader_;

  FirmwareLoader firmware_loader_;

  std::unique_ptr<SuspendResumeManager> suspend_resume_manager_;

  std::unique_ptr<DeviceManager> device_manager_;

  std::unique_ptr<CompositeNodeSpecManager> composite_node_spec_manager_;

  std::unique_ptr<BindDriverManager> bind_driver_manager_;

  uint32_t next_dfv2_device_id_ = 0;

  // This needs to outlive `coordinator` but should be destroyed in the same
  // event loop iteration. Otherwise, we risk use-after-free issues if a client
  // tries to connect to the servers published in it.
  component::OutgoingDirectory* outgoing_;
};

#endif  // SRC_DEVICES_BIN_DRIVER_MANAGER_V1_COORDINATOR_H_
