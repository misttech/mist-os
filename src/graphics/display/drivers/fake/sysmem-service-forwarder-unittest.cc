// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/drivers/fake/sysmem-service-forwarder.h"

#include <fidl/fuchsia.hardware.sysmem/cpp/fidl.h>
#include <fidl/fuchsia.io/cpp/fidl.h>
#include <fidl/fuchsia.sysmem/cpp/fidl.h>
#include <fidl/fuchsia.sysmem2/cpp/fidl.h>
#include <lib/component/incoming/cpp/service.h>
#include <lib/device-watcher/cpp/device-watcher.h>
#include <lib/fdio/directory.h>
#include <zircon/syscalls/object.h>

#include <string_view>

#include <gtest/gtest.h>

#include "src/lib/testing/predicates/status.h"

namespace display {

namespace {

zx::result<fidl::ClientEnd<fuchsia_io::Directory>> OpenSvcDirectory(
    const fidl::ClientEnd<fuchsia_io::Directory>& root_directory) {
  auto [svc_client, svc_server] = fidl::Endpoints<fuchsia_io::Directory>::Create();
  zx_status_t status = fdio_open_at(root_directory.handle()->get(), "/svc",
                                    static_cast<uint32_t>(fuchsia_io::OpenFlags::kDirectory),
                                    std::move(svc_server).TakeChannel().release());
  if (status != ZX_OK) {
    return zx::error(status);
  }
  return zx::ok(std::move(svc_client));
}

TEST(SysmemServiceForwarder, ServiceDirectoryContainsSysmemService) {
  zx::result<std::unique_ptr<SysmemServiceForwarder>> create_result =
      SysmemServiceForwarder::Create();
  ASSERT_OK(create_result);

  std::unique_ptr<SysmemServiceForwarder> sysmem_service_forwarder =
      std::move(create_result).value();

  zx::result<fidl::ClientEnd<fuchsia_io::Directory>> outgoing_directory_result =
      sysmem_service_forwarder->GetOutgoingDirectory();
  ASSERT_OK(outgoing_directory_result);

  fidl::ClientEnd<fuchsia_io::Directory> outgoing_directory =
      std::move(outgoing_directory_result).value();
  ASSERT_TRUE(outgoing_directory.is_valid());

  zx::result<fidl::ClientEnd<fuchsia_io::Directory>> svc_directory_result =
      OpenSvcDirectory(outgoing_directory);
  ASSERT_OK(svc_directory_result);
  fidl::ClientEnd<fuchsia_io::Directory> svc_directory = std::move(svc_directory_result).value();

  static const std::string kExpectedServiceName = "fuchsia.hardware.sysmem.Service";
  zx::result<> watch_svc_result = device_watcher::WatchDirectoryForItems(
      svc_directory, [](std::string_view name) -> std::optional<std::monostate> {
        if (name == kExpectedServiceName)
          return std::monostate{};
        return std::nullopt;
      });
  EXPECT_OK(watch_svc_result);
}

TEST(SysmemServiceForwarder, OpenAllocatorV1) {
  zx::result<std::unique_ptr<SysmemServiceForwarder>> create_result =
      SysmemServiceForwarder::Create();
  ASSERT_OK(create_result);

  std::unique_ptr<SysmemServiceForwarder> sysmem_service_forwarder =
      std::move(create_result).value();

  zx::result<fidl::ClientEnd<fuchsia_io::Directory>> outgoing_directory_result =
      sysmem_service_forwarder->GetOutgoingDirectory();
  ASSERT_OK(outgoing_directory_result);

  fidl::ClientEnd<fuchsia_io::Directory> outgoing_directory =
      std::move(outgoing_directory_result).value();
  ASSERT_TRUE(outgoing_directory.is_valid());

  zx::result<fidl::ClientEnd<fuchsia_io::Directory>> svc_directory_result =
      OpenSvcDirectory(outgoing_directory);
  ASSERT_OK(svc_directory_result);
  fidl::ClientEnd<fuchsia_io::Directory> svc_directory = std::move(svc_directory_result).value();

  zx::result<fidl::ClientEnd<fuchsia_sysmem::Allocator>> allocator_v1_result =
      component::ConnectAtMember<fuchsia_hardware_sysmem::Service::AllocatorV1>(svc_directory);
  ASSERT_OK(allocator_v1_result);

  fidl::SyncClient allocator_v1(std::move(allocator_v1_result).value());
  ASSERT_TRUE(allocator_v1.is_valid());

  // Make FIDL calls to make sure the Allocator V1 client is correctly connected to the component-
  // provided sysmem service.
  auto [collection_client, collection_server] =
      fidl::Endpoints<fuchsia_sysmem::BufferCollection>::Create();
  fit::result<fidl::OneWayStatus> allocate_collection_result =
      allocator_v1->AllocateNonSharedCollection(std::move(collection_server));
  ASSERT_TRUE(allocate_collection_result.is_ok())
      << allocate_collection_result.error_value().FormatDescription();

  fit::result<fidl::OneWayError> set_constraints_result =
      fidl::Call(collection_client)
          ->SetConstraints({{
              .has_constraints = true,
              .constraints = fuchsia_sysmem::BufferCollectionConstraints{{
                  .usage = fuchsia_sysmem::BufferUsage{{
                      .cpu = fuchsia_sysmem::kCpuUsageRead,
                  }},
                  .min_buffer_count_for_camping = 1,
                  .has_buffer_memory_constraints = true,
                  .buffer_memory_constraints = fuchsia_sysmem::BufferMemoryConstraints{{
                      .min_size_bytes = 4096,
                  }},
              }},
          }});
  ASSERT_TRUE(set_constraints_result.is_ok())
      << set_constraints_result.error_value().FormatDescription();

  fidl::Result allocation_result = fidl::Call(collection_client)->WaitForBuffersAllocated();
  ASSERT_TRUE(allocation_result.is_ok()) << allocation_result.error_value().FormatDescription();

  fit::result<fidl::OneWayError> close_result = fidl::Call(collection_client)->Close();
  ASSERT_TRUE(close_result.is_ok()) << allocation_result.error_value().FormatDescription();
}

TEST(SysmemServiceForwarder, OpenAllocatorV2) {
  zx::result<std::unique_ptr<SysmemServiceForwarder>> create_result =
      SysmemServiceForwarder::Create();
  ASSERT_OK(create_result);

  std::unique_ptr<SysmemServiceForwarder> sysmem_service_forwarder =
      std::move(create_result).value();

  zx::result<fidl::ClientEnd<fuchsia_io::Directory>> outgoing_directory_result =
      sysmem_service_forwarder->GetOutgoingDirectory();
  ASSERT_OK(outgoing_directory_result);

  fidl::ClientEnd<fuchsia_io::Directory> outgoing_directory =
      std::move(outgoing_directory_result).value();
  ASSERT_TRUE(outgoing_directory.is_valid());

  zx::result<fidl::ClientEnd<fuchsia_io::Directory>> svc_directory_result =
      OpenSvcDirectory(outgoing_directory);
  ASSERT_OK(svc_directory_result);
  fidl::ClientEnd<fuchsia_io::Directory> svc_directory = std::move(svc_directory_result).value();

  zx::result<fidl::ClientEnd<fuchsia_sysmem2::Allocator>> allocator_v2_result =
      component::ConnectAtMember<fuchsia_hardware_sysmem::Service::AllocatorV2>(svc_directory);
  ASSERT_OK(allocator_v2_result);

  fidl::SyncClient allocator_v2(std::move(allocator_v2_result).value());
  ASSERT_TRUE(allocator_v2.is_valid());

  // Make FIDL calls to make sure the Allocator V2 client is correctly connected to the component-
  // provided sysmem service.
  auto [collection_client, collection_server] =
      fidl::Endpoints<fuchsia_sysmem2::BufferCollection>::Create();
  fit::result<fidl::OneWayStatus> allocate_collection_result =
      allocator_v2->AllocateNonSharedCollection(
          {{.collection_request = std::move(collection_server)}});
  ASSERT_TRUE(allocate_collection_result.is_ok())
      << allocate_collection_result.error_value().FormatDescription();

  fit::result<fidl::OneWayError> set_constraints_result =
      fidl::Call(collection_client)
          ->SetConstraints({{
              .constraints = fuchsia_sysmem2::BufferCollectionConstraints{{
                  .usage = fuchsia_sysmem2::BufferUsage{{
                      .cpu = fuchsia_sysmem2::kCpuUsageRead,
                  }},
                  .min_buffer_count = 1,
                  .buffer_memory_constraints = fuchsia_sysmem2::BufferMemoryConstraints{{
                      .min_size_bytes = 4096,
                      .cpu_domain_supported = true,
                  }},
              }},
          }});
  ASSERT_TRUE(set_constraints_result.is_ok())
      << set_constraints_result.error_value().FormatDescription();

  fidl::Result allocation_result = fidl::Call(collection_client)->WaitForAllBuffersAllocated();
  ASSERT_TRUE(allocation_result.is_ok()) << allocation_result.error_value().FormatDescription();

  fit::result<fidl::OneWayError> release_result = fidl::Call(collection_client)->Release();
  ASSERT_TRUE(release_result.is_ok()) << allocation_result.error_value().FormatDescription();
}

}  // namespace

}  // namespace display
