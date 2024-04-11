// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.gpu.magma/cpp/wire.h>
#include <fidl/fuchsia.memorypressure/cpp/fidl.h>
#include <lib/async-loop/default.h>
#include <lib/async-loop/loop.h>
#include <lib/async/cpp/task.h>
#include <lib/fidl/cpp/wire/channel.h>
#include <lib/magma_service/sys_driver/dfv1/magma_dependency_injection_device.h>
#include <lib/sync/completion.h>

#include <gtest/gtest.h>

#include "lib/fidl/cpp/wire/channel.h"
#include "src/devices/testing/mock-ddk/mock-device.h"

namespace {
class TestOwner : public magma::MagmaDependencyInjectionDevice::Owner {
 public:
  // Will be called on an arbitrary thread.
  void SetMemoryPressureLevel(msd::MagmaMemoryPressureLevel level) override {
    level_ = level;
    sync_completion_signal(&completion_);
  }
  msd::MagmaMemoryPressureLevel level() const { return level_; }
  sync_completion_t& completion() { return completion_; }

 private:
  msd::MagmaMemoryPressureLevel level_{msd::MAGMA_MEMORY_PRESSURE_LEVEL_NORMAL};
  sync_completion_t completion_;
};

class Provider : public fidl::Server<fuchsia_memorypressure::Provider> {
 public:
  void RegisterWatcher(RegisterWatcherRequest& request,
                       RegisterWatcherCompleter::Sync& completer) override {
    fidl::SyncClient client(std::move(request.watcher()));
    client->OnLevelChanged(fidl::Request<fuchsia_memorypressure::Watcher::OnLevelChanged>{
        fuchsia_memorypressure::Level::kCritical});
  }

  fidl::ServerBindingGroup<fuchsia_memorypressure::Provider>& binding_set() { return binding_set_; }

 private:
  fidl::ServerBindingGroup<fuchsia_memorypressure::Provider> binding_set_;
};

TEST(DependencyInjection, Load) {
  async::Loop loop(&kAsyncLoopConfigNoAttachToCurrentThread);
  async::Loop fidl_loop(&kAsyncLoopConfigNoAttachToCurrentThread);
  auto parent = MockDevice::FakeRootParent();
  TestOwner owner;
  Provider provider;
  auto dependency_injection_device =
      std::make_unique<magma::MagmaDependencyInjectionDevice>(parent.get(), &owner);
  auto* device = dependency_injection_device.get();
  EXPECT_EQ(ZX_OK,
            magma::MagmaDependencyInjectionDevice::Bind(std::move(dependency_injection_device)));
  EXPECT_TRUE(device);
  auto* child = parent->GetLatestChild();

  auto endpoints = fidl::Endpoints<fuchsia_gpu_magma::DependencyInjection>::Create();
  std::optional<fidl::ServerBindingRef<fuchsia_gpu_magma::DependencyInjection>> binding =
      fidl::BindServer(fidl_loop.dispatcher(), std::move(endpoints.server),
                       static_cast<fidl::WireServer<fuchsia_gpu_magma::DependencyInjection>*>(
                           child->GetDeviceContext<magma::MagmaDependencyInjectionDevice>()));
  EXPECT_EQ(ZX_OK, fidl_loop.StartThread("fidl-server-thread"));

  EXPECT_EQ(ZX_OK, loop.StartThread("memory-pressure-thread"));

  fidl::WireSyncClient client{std::move(endpoints.client)};

  auto provider_endpoints = fidl::Endpoints<fuchsia_memorypressure::Provider>::Create();
  async::PostTask(loop.dispatcher(),
                  [&loop, server = std::move(provider_endpoints.server), &provider]() mutable {
                    provider.binding_set().AddBinding(loop.dispatcher(), std::move(server),
                                                      &provider, fidl::kIgnoreBindingClosure);
                  });

  EXPECT_EQ(ZX_OK,
            client->SetMemoryPressureProvider(std::move(provider_endpoints.client)).status());

  sync_completion_wait(&owner.completion(), ZX_TIME_INFINITE);
  EXPECT_EQ(owner.level(), msd::MAGMA_MEMORY_PRESSURE_LEVEL_CRITICAL);

  fidl_loop.Shutdown();

  device_async_remove(device->zxdev());
  mock_ddk::ReleaseFlaggedDevices(parent.get());

  // Ensure loop shutdown happens before |provider| is torn down.
  loop.Shutdown();
}
}  // namespace
