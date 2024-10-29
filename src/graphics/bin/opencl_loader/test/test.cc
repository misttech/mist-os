// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>
#include <fidl/fuchsia.gpu.magma/cpp/test_base.h>
#include <fidl/fuchsia.io/cpp/wire.h>
#include <fidl/fuchsia.opencl.loader/cpp/wire.h>
#include <fidl/fuchsia.sys2/cpp/wire.h>
#include <lib/fdio/directory.h>
#include <lib/fdio/namespace.h>
#include <lib/fit/defer.h>
#include <lib/fzl/vmo-mapper.h>
#include <lib/zx/vmo.h>
#include <zircon/status.h>
#include <zircon/types.h>

#include <filesystem>

#include <fbl/unique_fd.h>
#include <gtest/gtest.h>

// This is the first and only ICD loaded, so it should have a "0-" prepended.
const char kIcdFilename[] = "0-libopencl_fake.so";
const char kLoaderSvcPath[] = "/svc/fuchsia.opencl.loader.Loader";

class OpenclLoader : public testing::Test {
 protected:
  void SetUp() override {
    auto loader = ConnectToLoaderService();
    ASSERT_TRUE(loader.is_ok()) << loader.status_string();
    loader_ = std::move(*loader);
  }

  const auto& loader() const { return *loader_; }

  zx::result<zx::vmo> GetIcd(std::string_view icd_filename) {
    auto response = loader()->Get(fidl::StringView::FromExternal(icd_filename));
    if (!response.ok()) {
      return zx::error(response.status());
    }
    return zx::ok(std::move(response->lib));
  }

 private:
  static zx::result<fidl::WireSyncClient<fuchsia_opencl_loader::Loader>> ConnectToLoaderService() {
    zx::result loader_endpoints = fidl::CreateEndpoints<fuchsia_opencl_loader::Loader>();
    if (loader_endpoints.is_error()) {
      return loader_endpoints.take_error();
    }
    if (zx_status_t status =
            fdio_service_connect(kLoaderSvcPath, loader_endpoints->server.TakeHandle().release());
        status != ZX_OK) {
      return zx::error(status);
    }
    return zx::ok(std::move(loader_endpoints->client));
  }

  std::optional<fidl::WireSyncClient<fuchsia_opencl_loader::Loader>> loader_;
};

TEST_F(OpenclLoader, ManifestLoad) {
  zx::result icd = GetIcd(kIcdFilename);
  ASSERT_TRUE(icd.is_ok()) << icd.status_string();
  ASSERT_TRUE(icd->is_valid());
  zx_info_handle_basic_t handle_info;
  ASSERT_EQ(ZX_OK, icd->get_info(ZX_INFO_HANDLE_BASIC, &handle_info, sizeof(handle_info), nullptr,
                                 nullptr));
  EXPECT_TRUE(handle_info.rights & ZX_RIGHT_EXECUTE);
  EXPECT_FALSE(handle_info.rights & ZX_RIGHT_WRITE);

  zx::result not_present = GetIcd("not-present");
  ASSERT_TRUE(not_present.is_ok()) << not_present.status_string();
  EXPECT_FALSE(not_present->is_valid());
}

// Check that writes to one VMO returned by the server will not modify a separate VMO returned by
// the service.
TEST_F(OpenclLoader, VmosIndependent) {
  // manifest.json remaps this to bin/opencl-server.
  zx::result icd = GetIcd(kIcdFilename);
  ASSERT_TRUE(icd.is_ok()) << icd.status_string();
  ASSERT_TRUE(icd->is_valid());

  fzl::VmoMapper mapper;
  ASSERT_EQ(mapper.Map(*icd, 0, 0, ZX_VM_PERM_EXECUTE | ZX_VM_PERM_READ | ZX_VM_ALLOW_FAULTS),
            ZX_OK);
  uint8_t original_value = *static_cast<uint8_t*>(mapper.start());
  uint8_t byte_to_write = original_value + 1;
  size_t actual;
  // zx_process_write_memory can write to memory mapped without ZX_VM_PERM_WRITE. If that ever
  // changes, this test can probably be removed.
  zx_status_t status = zx::process::self()->write_memory(
      reinterpret_cast<uint64_t>(mapper.start()), &byte_to_write, sizeof(byte_to_write), &actual);

  // zx_process_write_memory may be disabled using a kernel command-line flag.
  if (status == ZX_ERR_NOT_SUPPORTED) {
    EXPECT_EQ(original_value, *static_cast<uint8_t*>(mapper.start()));
  } else {
    EXPECT_EQ(ZX_OK, status);

    EXPECT_EQ(byte_to_write, *static_cast<uint8_t*>(mapper.start()));
  }

  // Ensure that the new clone is unaffected.
  zx::result icd2 = GetIcd(kIcdFilename);
  ASSERT_TRUE(icd2.is_ok()) << icd.status_string();
  ASSERT_TRUE(icd2->is_valid());

  fzl::VmoMapper mapper2;
  ASSERT_EQ(mapper2.Map(*icd2, 0, 0, ZX_VM_PERM_EXECUTE | ZX_VM_PERM_READ | ZX_VM_ALLOW_FAULTS),
            ZX_OK);
  EXPECT_EQ(original_value, *static_cast<uint8_t*>(mapper2.start()));
}

TEST_F(OpenclLoader, DeviceFs) {
  auto dir = fidl::Endpoints<fuchsia_io::Directory>::Create();
  auto connect_result = loader()->ConnectToDeviceFs(dir.server.TakeChannel());
  ASSERT_TRUE(connect_result.ok()) << connect_result.status_string();

  ASSERT_TRUE(GetIcd(kIcdFilename).is_ok());  // Wait for idle.

  auto device = fidl::Endpoints<fuchsia_gpu_magma::Device>::Create();
  ASSERT_EQ(fdio_service_connect_at(dir.client.handle()->get(), "class/gpu/000",
                                    device.server.TakeHandle().release()),
            ZX_OK);

  auto response =
      fidl::WireCall(device.client)->Query(::fuchsia_gpu_magma::wire::QueryId::kVendorId);
  ASSERT_TRUE(response.ok()) << response.error();
  ASSERT_TRUE(response->is_ok()) << zx_status_get_string(response->error_value());
  ASSERT_TRUE(response->value()->is_simple_result());
  EXPECT_EQ(response->value()->simple_result(), 5u);
}

TEST_F(OpenclLoader, Features) {
  auto response = loader()->GetSupportedFeatures();
  ASSERT_TRUE(response.ok()) << response.error();
  constexpr fuchsia_opencl_loader::Features kExpectedFeatures =
      fuchsia_opencl_loader::Features::kConnectToDeviceFs | fuchsia_opencl_loader::Features::kGet;
  EXPECT_EQ(response.value().features, kExpectedFeatures);
}

TEST_F(OpenclLoader, ManifestFs) {
  auto manifest_fs = fidl::Endpoints<fuchsia_io::Directory>::Create();
  {
    auto response =
        loader()->ConnectToManifestFs(fuchsia_opencl_loader::ConnectToManifestOptions::kWaitForIdle,
                                      manifest_fs.server.TakeChannel());
    ASSERT_TRUE(response.ok()) << response;
  }

  fbl::unique_fd dir_fd;
  zx_status_t status =
      fdio_fd_create(manifest_fs.client.TakeChannel().release(), dir_fd.reset_and_get_address());
  ASSERT_EQ(status, ZX_OK) << zx_status_get_string(status);

  fbl::unique_fd manifest_fd(
      openat(dir_fd.get(), (std::string(kIcdFilename) + ".json").c_str(), O_RDONLY));
  ASSERT_TRUE(manifest_fd.is_valid()) << strerror(errno);

  constexpr int kManifestFileSize = 135;
  char manifest_data[kManifestFileSize + 1];
  ssize_t read_size = read(manifest_fd.get(), manifest_data, sizeof(manifest_data) - 1);
  EXPECT_EQ(kManifestFileSize, read_size);
}

TEST_F(OpenclLoader, DebugFilesystems) {
  ASSERT_TRUE(GetIcd(kIcdFilename).is_ok());  // Wait for idle.

  auto realm = fidl::Endpoints<fuchsia_sys2::RealmQuery>::Create();
  ASSERT_EQ(
      fdio_service_connect("/svc/fuchsia.sys2.RealmQuery", realm.server.TakeChannel().release()),
      ZX_OK);

  auto [outgoing_client, outgoing_server] = fidl::Endpoints<fuchsia_io::Directory>::Create();

  auto response = fidl::WireCall(realm.client)
                      ->OpenDirectory("./opencl_loader", fuchsia_sys2::OpenDirType::kOutgoingDir,
                                      std::move(outgoing_server));
  ASSERT_TRUE(response.ok()) << response;
  ASSERT_TRUE(response->is_ok()) << static_cast<uint32_t>(response->error_value());

  fdio_ns_t* ns;
  EXPECT_EQ(ZX_OK, fdio_ns_get_installed(&ns));
  EXPECT_EQ(ZX_OK, fdio_ns_bind(ns, "/loader_out", outgoing_client.TakeChannel().release()));
  auto cleanup_binding = fit::defer([&]() { fdio_ns_unbind(ns, "/loader_out"); });

  const std::string debug_path("/loader_out/debug/");

  EXPECT_TRUE(std::filesystem::exists(debug_path + "device-fs/class/gpu/000"));
  EXPECT_TRUE(std::filesystem::exists(debug_path + "manifest-fs/" + kIcdFilename + ".json"));
}
