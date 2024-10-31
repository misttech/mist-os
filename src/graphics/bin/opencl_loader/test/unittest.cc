// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.gpu.magma/cpp/test_base.h>
#include <fidl/fuchsia.io/cpp/wire.h>
#include <fidl/fuchsia.memorypressure/cpp/test_base.h>
#include <fidl/fuchsia.opencl.loader/cpp/test_base.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async/cpp/task.h>
#include <lib/fdio/namespace.h>
#include <lib/zx/vmo.h>

#include <fbl/unique_fd.h>
#include <gtest/gtest.h>

#include "src/graphics/bin/opencl_loader/app.h"
#include "src/graphics/bin/opencl_loader/icd_component.h"
#include "src/graphics/bin/opencl_loader/magma_dependency_injection.h"
#include "src/graphics/bin/opencl_loader/magma_device.h"
#include "src/lib/json_parser/json_parser.h"
#include "src/storage/lib/vfs/cpp/pseudo_dir.h"
#include "src/storage/lib/vfs/cpp/service.h"
#include "src/storage/lib/vfs/cpp/synchronous_vfs.h"
#include "src/storage/lib/vfs/cpp/vfs_types.h"

class LoaderUnittest : public ::testing::Test {
 protected:
  const inspect::Inspector& inspector() const { return inspector_; }

  LoaderApp* app() {
    if (!app_) {
      app_ = std::make_unique<LoaderApp>(&outgoing_dir_, dispatcher());
    }
    return app_.get();
  }

  async_dispatcher_t* dispatcher() const { return loop_.dispatcher(); }

  void RunLoopUntil(fit::function<bool()> condition) {
    while (!condition() && loop_.Run(zx::time::infinite(), true) == ZX_OK) {
    }
  }

  void TearDown() override {
    // We have to shutdown the loop before destroying app_ as some tasks may hold deferred actions
    // that reference the LoaderApp.
    loop_.Shutdown();
  }

 private:
  async::Loop loop_ = async::Loop(&kAsyncLoopConfigAttachToCurrentThread);
  inspect::Inspector inspector_;
  component::OutgoingDirectory outgoing_dir_ = component::OutgoingDirectory(dispatcher());
  std::unique_ptr<LoaderApp> app_;
};

class FakeMagmaDevice : public fidl::testing::TestBase<fuchsia_gpu_magma::CombinedDevice> {
 public:
  explicit FakeMagmaDevice(async_dispatcher_t* dispatcher) : dispatcher_(dispatcher) {}

  void CloseAll() { bindings_.CloseAll(ZX_OK); }

  auto ProtocolConnector() {
    return [this](fidl::ServerEnd<fuchsia_gpu_magma::CombinedDevice> server_end) -> zx_status_t {
      bindings_.AddBinding(dispatcher_, std::move(server_end), this, fidl::kIgnoreBindingClosure);
      return ZX_OK;
    };
  }

 private:
  void NotImplemented_(const std::string& name, ::fidl::CompleterBase& completer) override {
    ADD_FAILURE() << "unexpected call to " << name;
  }

  void GetIcdList(GetIcdListCompleter::Sync& completer) override {
    fuchsia_gpu_magma::IcdInfo info;
    info.component_url() = "a";
    info.flags() = fuchsia_gpu_magma::IcdFlags::kSupportsOpencl;
    std::vector<fuchsia_gpu_magma::IcdInfo> vec;
    vec.push_back(std::move(info));
    info.component_url() = "b";
    info.flags() = fuchsia_gpu_magma::IcdFlags::kSupportsVulkan;
    vec.push_back(std::move(info));
    completer.Reply(vec);
  }

  async_dispatcher_t* dispatcher_;
  fidl::ServerBindingGroup<fuchsia_gpu_magma::CombinedDevice> bindings_;
};

TEST_F(LoaderUnittest, MagmaDevice) {
  async::Loop vfs_loop(&kAsyncLoopConfigNoAttachToCurrentThread);
  fs::SynchronousVfs vfs(vfs_loop.dispatcher());
  FakeMagmaDevice magma_device(vfs_loop.dispatcher());
  auto root = fbl::MakeRefCounted<fs::PseudoDir>();
  const char* kDeviceNodeName = "dev";
  ASSERT_EQ(root->AddEntry(kDeviceNodeName,
                           fbl::MakeRefCounted<fs::Service>(magma_device.ProtocolConnector())),
            ZX_OK);
  vfs_loop.StartThread("vfs-loop");
  auto [client, server] = fidl::Endpoints<fuchsia_io::Directory>::Create();
  ASSERT_EQ(vfs.ServeDirectory(root, std::move(server), fuchsia_io::kRStarDir), ZX_OK);

  zx::result device = MagmaDevice::Create(app(), client, kDeviceNodeName, &inspector().GetRoot());
  ASSERT_TRUE(device.is_ok()) << device.status_string();
  auto* device_ptr = (*device).get();

  app()->AddDevice(std::move(*device));
  RunLoopUntil([device_ptr]() { return device_ptr->icd_count() > 0; });
  ASSERT_EQ(1u, app()->device_count());

  // Only 1 ICD listed supports OpenCL.
  const IcdList& icd_list = app()->devices()[0]->icd_list();
  EXPECT_EQ(1u, icd_list.ComponentCount());

  async::PostTask(vfs_loop.dispatcher(), [&magma_device]() { magma_device.CloseAll(); });
  RunLoopUntil([this]() { return app()->device_count() == 0; });
  EXPECT_EQ(0u, app()->device_count());
  vfs_loop.Shutdown();
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

class FakeMemoryPressureProvider
    : public fidl::testing::TestBase<fuchsia_memorypressure::Provider> {
 public:
  zx::result<fidl::ClientEnd<fuchsia_memorypressure::Provider>> Bind(
      async_dispatcher_t* dispatcher) {
    if (binding_) {
      return zx::error(ZX_ERR_ALREADY_BOUND);
    }
    zx::result endpoints = fidl::CreateEndpoints<fuchsia_memorypressure::Provider>();
    if (endpoints.is_error()) {
      return endpoints.take_error();
    }
    binding_ = fidl::BindServer(dispatcher, std::move(endpoints->server), this);
    return zx::ok(std::move(endpoints->client));
  }

  void Close() { binding_->Close(ZX_OK); }

 private:
  void NotImplemented_(const std::string& name, ::fidl::CompleterBase& completer) override {
    ADD_FAILURE() << "unexpected call to " << name;
  }

  void RegisterWatcher(RegisterWatcherRequest& request,
                       RegisterWatcherCompleter::Sync& completer) override {
    auto result =
        fidl::WireCall(request.watcher())->OnLevelChanged(fuchsia_memorypressure::Level::kCritical);
    if (!result.ok()) {
      GTEST_FAIL() << "Failed to set memory pressure level: " << result;
    }
  }

  std::optional<fidl::ServerBindingRef<fuchsia_memorypressure::Provider>> binding_;
};

class FakeMagmaDependencyInjection
    : public fidl::testing::TestBase<fuchsia_gpu_magma::DependencyInjection> {
 public:
  explicit FakeMagmaDependencyInjection(async_dispatcher_t* dispatcher) : dispatcher_(dispatcher) {}

  auto ProtocolConnector() {
    return [this](
               fidl::ServerEnd<fuchsia_gpu_magma::DependencyInjection> server_end) -> zx_status_t {
      bindings_.AddBinding(dispatcher_, std::move(server_end), this, fidl::kIgnoreBindingClosure);
      return ZX_OK;
    };
  }

  bool GotMemoryPressureProvider() const { return got_memory_pressure_provider_; }

  void CloseAll() { bindings_.CloseAll(ZX_OK); }

 private:
  void NotImplemented_(const std::string& name, ::fidl::CompleterBase& completer) override {
    ADD_FAILURE() << "unexpected call to " << name;
  }

  void SetMemoryPressureProvider(SetMemoryPressureProviderRequest& request,
                                 SetMemoryPressureProviderCompleter::Sync& completer) override {
    if (!request.provider().is_valid()) {
      GTEST_FAIL() << "Got invalid handle to fuchsia.memorypressure/Provider protocol.";
    }
    got_memory_pressure_provider_ = true;
  }

  async_dispatcher_t* dispatcher_;
  fidl::ServerBindingGroup<fuchsia_gpu_magma::DependencyInjection> bindings_;
  bool got_memory_pressure_provider_ = false;
};

TEST_F(LoaderUnittest, MagmaDependencyInjection) {
  FakeMemoryPressureProvider provider;

  fs::SynchronousVfs vfs(dispatcher());
  auto root = fbl::MakeRefCounted<fs::PseudoDir>();

  FakeMagmaDependencyInjection magma_dependency_injection(dispatcher());
  ASSERT_EQ(root->AddEntry("000", fbl::MakeRefCounted<fs::Service>(
                                      magma_dependency_injection.ProtocolConnector())),
            ZX_OK);
  auto gpu_dir = fidl::CreateEndpoints<fuchsia_io::Directory>();
  ASSERT_EQ(vfs.ServeDirectory(root, std::move(gpu_dir->server), fuchsia_io::kRStarDir), ZX_OK);

  fdio_ns_t* ns;
  EXPECT_EQ(ZX_OK, fdio_ns_get_installed(&ns));
  const char* kDependencyInjectionPath = "/dev/class/gpu-dependency-injection";
  EXPECT_EQ(ZX_OK,
            fdio_ns_bind(ns, kDependencyInjectionPath, gpu_dir->client.TakeChannel().release()));
  auto defer_unbind = fit::defer([&]() { fdio_ns_unbind(ns, kDependencyInjectionPath); });

  zx::result provider_client = provider.Bind(dispatcher());
  ASSERT_TRUE(provider_client.is_ok()) << provider_client.status_string();
  zx::result dependency_injection = MagmaDependencyInjection::Create(std::move(*provider_client));
  ASSERT_TRUE(dependency_injection.is_ok()) << dependency_injection.status_string();

  // Wait for the GPU dependency injection code to detect the device and call the method on it.
  RunLoopUntil([&magma_dependency_injection]() {
    return magma_dependency_injection.GotMemoryPressureProvider();
  });
}
