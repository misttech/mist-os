// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fuchsia/gpu/magma/cpp/fidl_test_base.h>
#include <fuchsia/io/cpp/fidl.h>
#include <fuchsia/memorypressure/cpp/fidl_test_base.h>
#include <fuchsia/opencl/loader/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async/cpp/task.h>
#include <lib/fdio/namespace.h>
#include <lib/fidl/cpp/binding_set.h>
#include <lib/fidl/cpp/interface_handle.h>
#include <lib/sys/cpp/component_context.h>
#include <lib/sys/cpp/testing/component_context_provider.h>
#include <lib/vfs/cpp/pseudo_dir.h>
#include <lib/vfs/cpp/service.h>
#include <lib/zx/vmo.h>

#include <fbl/unique_fd.h>
#include <gtest/gtest.h>

#include "src/graphics/bin/opencl_loader/app.h"
#include "src/graphics/bin/opencl_loader/icd_component.h"
#include "src/graphics/bin/opencl_loader/magma_dependency_injection.h"
#include "src/graphics/bin/opencl_loader/magma_device.h"
#include "src/lib/json_parser/json_parser.h"
#include "src/lib/storage/vfs/cpp/pseudo_dir.h"
#include "src/lib/storage/vfs/cpp/service.h"
#include "src/lib/storage/vfs/cpp/synchronous_vfs.h"
#include "src/lib/storage/vfs/cpp/vfs_types.h"
#include "src/lib/testing/loop_fixture/real_loop_fixture.h"

class LoaderUnittest : public gtest::RealLoopFixture {};

class FakeMagmaDevice : public fuchsia::gpu::magma::testing::CombinedDevice_TestBase {
 public:
  void NotImplemented_(const std::string& name) override { EXPECT_TRUE(false) << name; }
  void GetIcdList(GetIcdListCallback callback) override {
    fuchsia::gpu::magma::IcdInfo info;
    info.set_component_url("a");
    info.set_flags(fuchsia::gpu::magma::IcdFlags::SUPPORTS_OPENCL);
    std::vector<fuchsia::gpu::magma::IcdInfo> vec;
    vec.push_back(std::move(info));
    info.set_component_url("b");
    info.set_flags(fuchsia::gpu::magma::IcdFlags::SUPPORTS_VULKAN);
    vec.push_back(std::move(info));
    callback(std::move(vec));
  }
  fidl::InterfaceRequestHandler<fuchsia::gpu::magma::CombinedDevice> GetHandler() {
    return bindings_.GetHandler(this);
  }

  void CloseAll() { bindings_.CloseAll(); }

 private:
  fidl::BindingSet<fuchsia::gpu::magma::CombinedDevice> bindings_;
};

TEST_F(LoaderUnittest, MagmaDevice) {
  inspect::Inspector inspector;
  auto context = sys::ComponentContext::Create();
  LoaderApp app(context.get(), dispatcher());

  vfs::PseudoDir root;
  FakeMagmaDevice magma_device;
  const char* kDeviceNodeName = "dev";
  root.AddEntry(kDeviceNodeName, std::make_unique<vfs::Service>(magma_device.GetHandler()));
  zx::result endpoints = fidl::CreateEndpoints<fuchsia_io::Directory>();
  ASSERT_TRUE(endpoints.is_ok()) << endpoints.status_string();
  auto& [client, server] = endpoints.value();
  async::Loop vfs_loop(&kAsyncLoopConfigNoAttachToCurrentThread);
  vfs_loop.StartThread("vfs-loop");
  root.Serve(fuchsia::io::OpenFlags::RIGHT_READABLE, server.TakeChannel(), vfs_loop.dispatcher());

  auto device = MagmaDevice::Create(&app, client, kDeviceNodeName, &inspector.GetRoot());
  EXPECT_TRUE(device);
  auto device_ptr = device.get();

  app.AddDevice(std::move(device));
  RunLoopUntil([&device_ptr]() { return device_ptr->icd_count() > 0; });
  EXPECT_EQ(1u, app.device_count());

  // Only 1 ICD listed supports OpenCL.
  EXPECT_EQ(1u, static_cast<MagmaDevice*>(app.devices()[0].get())->icd_list().ComponentCount());

  async::PostTask(vfs_loop.dispatcher(), [&magma_device]() { magma_device.CloseAll(); });
  RunLoopUntil([&app]() { return app.device_count() == 0; });
  EXPECT_EQ(0u, app.device_count());
}

TEST(Icd, BadMetadata) {
  json::JSONParser parser;
  auto good_doc = parser.ParseFromString(R"({
    "file_path": "bin/opencl-server",
    "version": 1,
    "manifest_path": "data"
})",
                                         "test1");
  EXPECT_TRUE(IcdComponent::ValidateMetadataJson("a", good_doc));

  auto bad_doc1 = parser.ParseFromString(R"({
    "file_path": "bin/opencl-server",
    "version": 2,
    "manifest_path": "data"
})",
                                         "tests2");
  EXPECT_FALSE(IcdComponent::ValidateMetadataJson("b", bad_doc1));

  auto bad_doc2 = parser.ParseFromString(R"({
    "version": 1,
    "manifest_path": "data"
})",
                                         "test3");
  EXPECT_FALSE(IcdComponent::ValidateMetadataJson("c", bad_doc2));

  auto bad_doc3 = parser.ParseFromString(R"({
    "file_path": 1,
    "version": 1,
    "manifest_path": "data"
})",
                                         "tests4");
  EXPECT_FALSE(IcdComponent::ValidateMetadataJson("d", bad_doc3));
}

TEST(Icd, BadManifest) {
  json::JSONParser parser;
  auto good_doc = parser.ParseFromString(R"(
{
    "ICD": {
        "api_version": "1.1.0",
        "library_path": "libopencl_fake.so"
    },
    "file_format_version": "1.0.0"
})",
                                         "test1");
  EXPECT_TRUE(IcdComponent::ValidateManifestJson("a", good_doc));

  auto bad_doc1 = parser.ParseFromString(R"(
{
    "ICD": {
        "api_version": "1.1.0",
    },
    "file_format_version": "1.0.0"
})",
                                         "test1");
  EXPECT_FALSE(IcdComponent::ValidateManifestJson("a", bad_doc1));
}

class FakeMemoryPressureProvider : public fuchsia::memorypressure::testing::Provider_TestBase {
 public:
  void NotImplemented_(const std::string& name) override { EXPECT_TRUE(false) << name; }
  void RegisterWatcher(fidl::InterfaceHandle<::fuchsia::memorypressure::Watcher> watcher) override {
    fuchsia::memorypressure::WatcherSyncPtr watcher_sync;
    watcher_sync.Bind(std::move(watcher));
    watcher_sync->OnLevelChanged(fuchsia::memorypressure::Level::CRITICAL);
  }

  fidl::InterfaceRequestHandler<fuchsia::memorypressure::Provider> GetHandler() {
    return bindings_.GetHandler(this);
  }

  void CloseAll() { bindings_.CloseAll(); }

 private:
  fidl::BindingSet<fuchsia::memorypressure::Provider> bindings_;
};

class FakeMagmaDependencyInjection
    : public fuchsia::gpu::magma::testing::DependencyInjection_TestBase {
 public:
  void NotImplemented_(const std::string& name) override { EXPECT_TRUE(false) << name; }

  void SetMemoryPressureProvider(
      fidl::InterfaceHandle<fuchsia::memorypressure::Provider> provider) override {
    if (provider.is_valid()) {
      got_memory_pressure_provider_ = true;
    }
  }

  bool GotMemoryPressureProvider() const { return got_memory_pressure_provider_; }

  fidl::InterfaceRequestHandler<fuchsia::gpu::magma::DependencyInjection> GetHandler() {
    return bindings_.GetHandler(this);
  }

  void CloseAll() { bindings_.CloseAll(); }

 private:
  fidl::BindingSet<fuchsia::gpu::magma::DependencyInjection> bindings_;
  bool got_memory_pressure_provider_ = false;
};

TEST_F(LoaderUnittest, MagmaDependencyInjection) {
  sys::testing::ComponentContextProvider context_provider(dispatcher());
  FakeMemoryPressureProvider provider;
  context_provider.service_directory_provider()->AddService(provider.GetHandler());

  fs::SynchronousVfs vfs(dispatcher());
  auto root = fbl::MakeRefCounted<fs::PseudoDir>();

  FakeMagmaDependencyInjection magma_dependency_injection;
  root->AddEntry(
      "000", fbl::MakeRefCounted<fs::Service>([&magma_dependency_injection](zx::channel channel) {
        magma_dependency_injection.GetHandler()(
            fidl::InterfaceRequest<fuchsia::gpu::magma::DependencyInjection>(std::move(channel)));
        return ZX_OK;
      }));
  fidl::InterfaceHandle<fuchsia::io::Directory> gpu_dir;
  EXPECT_EQ(ZX_OK,
            vfs.ServeDirectory(
                root, fidl::ServerEnd<fuchsia_io::Directory>{gpu_dir.NewRequest().TakeChannel()},
                fs::Rights::ReadOnly()));

  fdio_ns_t* ns;
  EXPECT_EQ(ZX_OK, fdio_ns_get_installed(&ns));
  const char* kDependencyInjectionPath = "/dev/class/gpu-dependency-injection";
  EXPECT_EQ(ZX_OK, fdio_ns_bind(ns, kDependencyInjectionPath, gpu_dir.TakeChannel().release()));
  auto defer_unbind = fit::defer([&]() { fdio_ns_unbind(ns, kDependencyInjectionPath); });

  MagmaDependencyInjection dependency_injection(context_provider.context());
  EXPECT_EQ(ZX_OK, dependency_injection.Initialize());

  // Wait for the GPU dependency injection code to detect the device and call the method on it.
  RunLoopUntil([&magma_dependency_injection]() {
    return magma_dependency_injection.GotMemoryPressureProvider();
  });
}
