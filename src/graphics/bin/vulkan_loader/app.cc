// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/bin/vulkan_loader/app.h"

#include <fidl/fuchsia.component/cpp/wire.h>
#include <fidl/fuchsia.io/cpp/wire.h>
#include <lib/async-loop/default.h>
#include <lib/async/cpp/task.h>
#include <lib/component/incoming/cpp/service.h>
#include <lib/fdio/directory.h>

#include "src/graphics/bin/vulkan_loader/goldfish_device.h"
#include "src/graphics/bin/vulkan_loader/icd_component.h"
#include "src/graphics/bin/vulkan_loader/lavapipe_device.h"
#include "src/graphics/bin/vulkan_loader/magma_device.h"
#include "src/storage/lib/vfs/cpp/remote_dir.h"

namespace {
zx::result<fidl::ClientEnd<fuchsia_component::Realm>> GetRealmClient() {
  auto endpoints = fidl::CreateEndpoints<fuchsia_component::Realm>();
  if (endpoints.is_error()) {
    FX_LOGS(INFO) << "Failed to create endpoints: " << endpoints.status_string();
    return endpoints.take_error();
  }
  if (auto result = component::Connect(std::move(endpoints->server)); result.is_error()) {
    return result.take_error();
  }
  return zx::ok(std::move(endpoints->client));
}
}  // namespace

LoaderApp::LoaderApp(component::OutgoingDirectory* outgoing_dir, async_dispatcher_t* dispatcher,
                     structured_config_lib::Config structured_config)
    : outgoing_dir_(outgoing_dir),
      dispatcher_(dispatcher),
      inspector_(dispatcher, {}),
      debug_fs_(dispatcher),
      fdio_loop_(&kAsyncLoopConfigNoAttachToCurrentThread) {
  fdio_loop_.StartThread("fdio_loop");
  inspector_.Health().StartingUp();
  config_node_ = inspector_.root().CreateChild("config");
  devices_node_ = inspector_.root().CreateChild("devices");
  icds_node_ = inspector_.root().CreateChild("icds");

  debug_root_node_ = fbl::MakeRefCounted<fs::PseudoDir>();
  device_root_node_ = fbl::MakeRefCounted<fs::PseudoDir>();
  manifest_fs_root_node_ = fbl::MakeRefCounted<fs::PseudoDir>();

  allow_magma_icds_ = structured_config.allow_magma_icds();
  allow_goldfish_icd_ = structured_config.allow_goldfish_icd();
  allow_lavapipe_icd_ = structured_config.allow_lavapipe_icd();
  lavapipe_icd_url_ = structured_config.lavapipe_icd_url();

  structured_config.RecordInspect(&config_node_);
}
LoaderApp::~LoaderApp() = default;

zx_status_t LoaderApp::InitDeviceFs() {
  const char* kDevClassList[] = {"gpu", "goldfish-pipe", "goldfish-control",
                                 "goldfish-address-space", "goldfish-sync"};

  auto class_node = fbl::MakeRefCounted<fs::PseudoDir>();
  ZX_ASSERT(device_root_node_->AddEntry("class", class_node) == ZX_OK);

  for (const char* dev_class : kDevClassList) {
    auto endpoints = fidl::Endpoints<fuchsia_io::Directory>::Create();
    std::string input_path = std::string("/dev/class/") + dev_class;

    // NB: RIGHT_READABLE is needed here because downstream code in vulkan will attempt to open this
    // directory using POSIX APIs which cannot express opening without any rights.
    zx_status_t status =
        fdio_open3(input_path.c_str(), static_cast<uint64_t>(fuchsia_io::kPermReadable),
                   endpoints.server.TakeChannel().release());
    if (status != ZX_OK) {
      FX_PLOGS(ERROR, status) << "Failed to open " << input_path;
      return status;
    }

    ZX_ASSERT(class_node->AddEntry(dev_class, fbl::MakeRefCounted<fs::RemoteDir>(
                                                  std::move(endpoints.client))) == ZX_OK);
  }

  return ZX_OK;
}

zx_status_t LoaderApp::ServeDeviceFs(fidl::ServerEnd<fuchsia_io::Directory> server_end) {
  return debug_fs_.ServeDirectory(device_root_node_, std::move(server_end), fuchsia_io::kRStarDir);
}

zx_status_t LoaderApp::ServeManifestFs(fidl::ServerEnd<fuchsia_io::Directory> server_end) {
  return debug_fs_.ServeDirectory(manifest_fs_root_node_, std::move(server_end),
                                  fuchsia_io::kRStarDir);
}

zx_status_t LoaderApp::InitDebugFs() {
  if (zx_status_t status = InitDeviceFs(); status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "Failed to initialize device-fs: ";
    return status;
  }
  ZX_ASSERT(debug_root_node_->AddEntry("device-fs", device_root_node_) == ZX_OK);
  ZX_ASSERT(debug_root_node_->AddEntry("manifest-fs", manifest_fs_root_node_) == ZX_OK);
  auto endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
  if (zx_status_t status = debug_fs_.ServeDirectory(debug_root_node_, std::move(endpoints->server),
                                                    fuchsia_io::kRStarDir);
      status != ZX_OK) {
    FX_PLOGS(ERROR, status) << "Failed to serve debug filesystem: ";
    return status;
  }
  if (auto result = outgoing_dir_->AddDirectory(std::move(endpoints->client), "debug");
      result.is_error()) {
    FX_LOGS(ERROR) << "Failed to add debug filesystem to outgoing directory: "
                   << result.status_string();
    return result.status_value();
  }
  return ZX_OK;
}

zx_status_t LoaderApp::InitDeviceWatcher() {
  FIT_DCHECK_IS_THREAD_VALID(main_thread_);

  if (allow_magma_icds_) {
    auto gpu_watcher_token = GetPendingActionToken();
    gpu_watcher_ = fsl::DeviceWatcher::CreateWithIdleCallback(
        "/dev/class/gpu",
        [this](const fidl::ClientEnd<fuchsia_io::Directory>& dir, const std::string& filename) {
          if (filename == ".") {
            return;
          }
          zx::result device = MagmaDevice::Create(this, dir, filename, &devices_node_);
          if (device.is_ok()) {
            devices_.emplace_back(std::move(*device));
          } else {
            FX_LOGS(ERROR) << "Failed to create MagmaDevice: " << device.status_string();
          }
        },
        [gpu_watcher_token = std::move(gpu_watcher_token)]() {
          // Idle callback and gpu_watcher_token will be destroyed on idle.
        },
        dispatcher_);
    if (!gpu_watcher_)
      return ZX_ERR_INTERNAL;
  }

  if (allow_goldfish_icd_) {
    auto goldfish_watcher_token = GetPendingActionToken();
    goldfish_watcher_ = fsl::DeviceWatcher::CreateWithIdleCallback(
        "/dev/class/goldfish-pipe",
        [this](const fidl::ClientEnd<fuchsia_io::Directory>& dir, const std::string& filename) {
          if (filename == ".") {
            return;
          }
          auto device = GoldfishDevice::Create(this, dir, filename, &devices_node_);
          if (device) {
            devices_.emplace_back(std::move(device));
          }
        },
        [goldfish_watcher_token = std::move(goldfish_watcher_token)]() {
          // Idle callback and goldfish_watcher_token will be destroyed on idle.
        },
        dispatcher_);
    if (!goldfish_watcher_)
      return ZX_ERR_INTERNAL;
  }

  if (allow_lavapipe_icd_) {
    auto device = LavapipeDevice::Create(this, "0", &devices_node_, lavapipe_icd_url_);
    if (device) {
      devices_.emplace_back(std::move(device));
    }
  }

  return ZX_OK;
}

void LoaderApp::RemoveDevice(GpuDevice* device) {
  FIT_DCHECK_IS_THREAD_VALID(main_thread_);
  auto it = std::remove_if(devices_.begin(), devices_.end(),
                           [device](const std::unique_ptr<GpuDevice>& unique_device) {
                             return device == unique_device.get();
                           });
  devices_.erase(it, devices_.end());
}

zx::result<std::shared_ptr<IcdComponent>> LoaderApp::CreateIcdComponent(
    const std::string& component_url) {
  FIT_DCHECK_IS_THREAD_VALID(main_thread_);
  if (icd_components_.find(component_url) != icd_components_.end()) {
    return zx::ok(icd_components_[component_url]);
  }
  zx::result realm_client = GetRealmClient();
  if (realm_client.is_error()) {
    FX_LOGS(ERROR) << "Failed to connect to Realm protocol: " << realm_client.status_string();
    return realm_client.take_error();
  }
  auto [it, inserted] = icd_components_.emplace(
      component_url,
      IcdComponent::Create(this, std::move(*realm_client), &icds_node_, component_url));
  return zx::ok(it->second);
}

void LoaderApp::NotifyIcdsChanged() {
  // This can be called on any thread.
  std::lock_guard lock(pending_action_mutex_);
  NotifyIcdsChangedLocked();
}

void LoaderApp::NotifyIcdsChangedLocked() {
  if (icd_notification_pending_)
    return;
  icd_notification_pending_ = true;
  async::PostTask(dispatcher_, [this]() { this->NotifyIcdsChangedOnMainThread(); });
}

void LoaderApp::NotifyIcdsChangedOnMainThread() {
  FIT_DCHECK_IS_THREAD_VALID(main_thread_);
  {
    std::lock_guard lock(pending_action_mutex_);
    icd_notification_pending_ = false;
  }
  bool have_icd = false;
  for (auto& device : devices_) {
    have_icd |= device->icd_list().UpdateCurrentComponent();
  }
  if (have_icd) {
    inspector_.Health().Ok();
  }
  for (auto& observer : observer_list_)
    observer.OnIcdListChanged(this);
}

std::optional<zx::vmo> LoaderApp::GetMatchingIcd(const std::string& name) {
  FIT_DCHECK_IS_THREAD_VALID(main_thread_);
  for (auto& device : devices_) {
    auto res = device->icd_list().GetVmoMatchingSystemLib(name);
    if (res) {
      return std::optional<zx::vmo>(std::move(res));
    }
  }
  {
    std::lock_guard lock(pending_action_mutex_);
    // If not actions are pending then assume there will never be a match.
    if (pending_action_count_ == 0) {
      return zx::vmo();
    }
  }
  return {};
}

std::unique_ptr<LoaderApp::PendingActionToken> LoaderApp::GetPendingActionToken() {
  return std::unique_ptr<PendingActionToken>(new PendingActionToken(this));
}

LoaderApp::PendingActionToken::~PendingActionToken() {
  std::lock_guard lock(app_->pending_action_mutex_);
  if (--app_->pending_action_count_ == 0) {
    app_->NotifyIcdsChangedLocked();
  }
}
