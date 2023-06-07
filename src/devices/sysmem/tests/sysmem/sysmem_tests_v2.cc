// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>
#include <fidl/fuchsia.sysmem2/cpp/fidl.h>
#include <lib/component/incoming/cpp/protocol.h>
#include <lib/fdio/cpp/caller.h>
#include <lib/fdio/fdio.h>
#include <lib/fdio/watcher.h>
#include <lib/fit/defer.h>
#include <lib/sysmem-version/sysmem-version.h>
#include <lib/zbi-format/graphics.h>
#include <lib/zbitl/items/graphics.h>
#include <lib/zx/clock.h>

#include <random>

#include <fbl/algorithm.h>
#include <fbl/unique_fd.h>
#include <zxtest/zxtest.h>

#include "common.h"
#include "secure_vmo_read_tester.h"
#include "test_observer.h"

namespace {

namespace v2 = fuchsia_sysmem2;

using TokenV2 = fidl::SyncClient<v2::BufferCollectionToken>;
using CollectionV2 = fidl::SyncClient<v2::BufferCollection>;
using GroupV2 = fidl::SyncClient<v2::BufferCollectionTokenGroup>;
using SharedTokenV2 = std::shared_ptr<TokenV2>;
using SharedCollectionV2 = std::shared_ptr<CollectionV2>;
using SharedGroupV2 = std::shared_ptr<GroupV2>;

zx::result<fidl::SyncClient<fuchsia_sysmem2::Allocator>> connect_to_sysmem_service_v2();
zx_status_t verify_connectivity_v2(fidl::SyncClient<fuchsia_sysmem2::Allocator>& allocator);

zx::result<fidl::SyncClient<fuchsia_sysmem2::Allocator>> connect_to_sysmem_service_v2() {
  auto client_end = component::Connect<fuchsia_sysmem2::Allocator>();
  EXPECT_OK(client_end);
  if (!client_end.is_ok()) {
    return zx::error(client_end.status_value());
  }
  fidl::SyncClient allocator{std::move(client_end.value())};
  fuchsia_sysmem2::AllocatorSetDebugClientInfoRequest request;
  request.name() = current_test_name;
  request.id() = 0u;
  auto result = allocator->SetDebugClientInfo(std::move(request));
  EXPECT_TRUE(result.is_ok());
  return zx::ok(std::move(allocator));
}

zx::result<fidl::SyncClient<fuchsia_sysmem2::Allocator>> connect_to_sysmem_driver_v2() {
  fbl::unique_fd sysmem_dir(open(SYSMEM_CLASS_PATH, O_RDONLY));

  zx::result<fidl::ClientEnd<fuchsia_sysmem2::DriverConnector>> client_end;
  zx_status_t status = fdio_watch_directory(
      sysmem_dir.get(),
      [](int dirfd, int event, const char* fn, void* cookie) {
        if (std::string_view{fn} == ".") {
          return ZX_OK;
        }
        if (event != WATCH_EVENT_ADD_FILE) {
          return ZX_OK;
        }
        fdio_cpp::UnownedFdioCaller caller(dirfd);
        *reinterpret_cast<zx::result<fidl::ClientEnd<fuchsia_sysmem2::DriverConnector>>*>(cookie) =
            component::ConnectAt<fuchsia_sysmem2::DriverConnector>(caller.directory(), fn);
        return ZX_ERR_STOP;
      },
      ZX_TIME_INFINITE, &client_end);
  EXPECT_STATUS(status, ZX_ERR_STOP);
  if (status != ZX_ERR_STOP) {
    if (status == ZX_OK) {
      status = ZX_ERR_BAD_STATE;
    }
    return zx::error(status);
  }
  EXPECT_OK(client_end);
  if (!client_end.is_ok()) {
    return zx::error(client_end.status_value());
  }
  fidl::WireSyncClient connector{std::move(client_end.value())};

  auto allocator_endpoints = fidl::CreateEndpoints<fuchsia_sysmem2::Allocator>();
  EXPECT_OK(allocator_endpoints);
  if (!allocator_endpoints.is_ok()) {
    return zx::error(allocator_endpoints.status_value());
  }

  auto connect_result = connector->ConnectV2(std::move(allocator_endpoints->server));
  EXPECT_OK(connect_result);
  if (!connect_result.ok()) {
    return zx::error(connect_result.status());
  }

  fidl::SyncClient allocator{std::move(allocator_endpoints->client)};
  fuchsia_sysmem2::AllocatorSetDebugClientInfoRequest request;
  request.name() = current_test_name;
  request.id() = 0u;
  auto result = allocator->SetDebugClientInfo(std::move(request));
  EXPECT_TRUE(result.is_ok());
  return zx::ok(std::move(allocator));
}

zx_status_t verify_connectivity_v2(fidl::SyncClient<fuchsia_sysmem2::Allocator>& allocator) {
  zx::result collection_endpoints = fidl::CreateEndpoints<fuchsia_sysmem2::BufferCollection>();
  EXPECT_TRUE(collection_endpoints.is_ok());
  if (!collection_endpoints.is_ok()) {
    return collection_endpoints.status_value();
  }
  auto [collection_client_end, collection_server_end] = std::move(*collection_endpoints);

  fuchsia_sysmem2::AllocatorAllocateNonSharedCollectionRequest request;
  request.collection_request().emplace(std::move(collection_server_end));
  auto result = allocator->AllocateNonSharedCollection(std::move(request));
  EXPECT_TRUE(result.is_ok());
  if (result.is_error()) {
    return result.error_value().status();
  }

  fidl::SyncClient collection(std::move(collection_client_end));
  auto sync_result = collection->Sync();
  EXPECT_TRUE(sync_result.is_ok());
  if (sync_result.is_error()) {
    return sync_result.error_value().status();
  }
  return ZX_OK;
}

static void SetDefaultCollectionNameV2(
    fidl::SyncClient<fuchsia_sysmem2::BufferCollection>& collection, fbl::String suffix = "") {
  constexpr uint32_t kPriority = 1000000;
  fbl::String name = "sysmem-test-v2";
  if (!suffix.empty()) {
    name = fbl::String::Concat({name, "-", suffix});
  }
  fuchsia_sysmem2::NodeSetNameRequest request;
  request.name() = name.c_str();
  request.priority() = kPriority;
  EXPECT_TRUE(collection->SetName(std::move(request)).is_ok());
}

zx::result<fidl::SyncClient<fuchsia_sysmem2::BufferCollection>>
make_single_participant_collection_v2() {
  // We could use AllocateNonSharedCollection() to implement this function, but we're already
  // using AllocateNonSharedCollection() during verify_connectivity_v1(), so instead just set up the
  // more general (and more real) way here.

  auto allocator = connect_to_sysmem_driver_v2();
  EXPECT_OK(allocator.status_value());
  if (!allocator.is_ok()) {
    return zx::error(allocator.status_value());
  }

  zx::result token_endpoints = fidl::CreateEndpoints<fuchsia_sysmem2::BufferCollectionToken>();
  EXPECT_TRUE(token_endpoints.is_ok());
  if (!token_endpoints.is_ok()) {
    return zx::error(token_endpoints.status_value());
  }
  auto [token_client_end, token_server_end] = std::move(*token_endpoints);

  fuchsia_sysmem2::AllocatorAllocateSharedCollectionRequest allocate_shared_request;
  allocate_shared_request.token_request() = std::move(token_server_end);
  auto new_collection_result =
      allocator->AllocateSharedCollection(std::move(allocate_shared_request));
  EXPECT_TRUE(new_collection_result.is_ok());
  if (!new_collection_result.is_ok()) {
    return zx::error(new_collection_result.error_value().status());
  }

  zx::result collection_endpoints = fidl::CreateEndpoints<fuchsia_sysmem2::BufferCollection>();
  EXPECT_TRUE(collection_endpoints.is_ok());
  if (!collection_endpoints.is_ok()) {
    return zx::error(collection_endpoints.status_value());
  }
  auto [collection_client_end, collection_server_end] = std::move(*collection_endpoints);

  EXPECT_NE(token_client_end.channel().get(), ZX_HANDLE_INVALID);
  fuchsia_sysmem2::AllocatorBindSharedCollectionRequest bind_shared_request;
  bind_shared_request.token() = std::move(token_client_end);
  bind_shared_request.buffer_collection_request() = std::move(collection_server_end);
  auto bind_result = allocator->BindSharedCollection(std::move(bind_shared_request));
  EXPECT_TRUE(bind_result.is_ok());
  if (!bind_result.is_ok()) {
    return zx::error(bind_result.error_value().status());
  }

  fidl::SyncClient collection{std::move(collection_client_end)};

  SetDefaultCollectionNameV2(collection);

  return zx::ok(std::move(collection));
}

fidl::SyncClient<v2::BufferCollectionToken> create_initial_token_v2() {
  zx::result allocator = connect_to_sysmem_service_v2();
  EXPECT_TRUE(allocator.is_ok());
  zx::result token_endpoints_0 = fidl::CreateEndpoints<v2::BufferCollectionToken>();
  EXPECT_TRUE(token_endpoints_0.is_ok());
  auto& [token_client_0, token_server_0] = token_endpoints_0.value();
  v2::AllocatorAllocateSharedCollectionRequest allocate_shared_request;
  allocate_shared_request.token_request() = std::move(token_server_0);
  EXPECT_TRUE(allocator->AllocateSharedCollection(std::move(allocate_shared_request)).is_ok());
  fidl::SyncClient token{std::move(token_client_0)};
  EXPECT_TRUE(token->Sync().is_ok());
  return token;
}

std::vector<fidl::SyncClient<v2::BufferCollection>> create_clients_v2(uint32_t client_count) {
  std::vector<fidl::SyncClient<v2::BufferCollection>> result;
  auto next_token = create_initial_token_v2();
  auto allocator = connect_to_sysmem_service_v2();
  for (uint32_t i = 0; i < client_count; ++i) {
    auto cur_token = std::move(next_token);
    if (i < client_count - 1) {
      zx::result token_endpoints = fidl::CreateEndpoints<v2::BufferCollectionToken>();
      EXPECT_TRUE(token_endpoints.is_ok());
      auto& [token_client_endpoint, token_server_endpoint] = token_endpoints.value();

      v2::BufferCollectionTokenDuplicateRequest duplicate_request;
      duplicate_request.rights_attenuation_mask() = ZX_RIGHT_SAME_RIGHTS;
      duplicate_request.token_request() = std::move(token_server_endpoint);
      EXPECT_TRUE(cur_token->Duplicate(std::move(duplicate_request)).is_ok());

      next_token = fidl::SyncClient(std::move(token_client_endpoint));
    }
    zx::result collection_endpoints = fidl::CreateEndpoints<v2::BufferCollection>();
    EXPECT_TRUE(collection_endpoints.is_ok());
    auto& [collection_client_endpoint, collection_server_endpoint] = collection_endpoints.value();

    v2::AllocatorBindSharedCollectionRequest bind_shared_request;
    bind_shared_request.token() = cur_token.TakeClientEnd();
    bind_shared_request.buffer_collection_request() = std::move(collection_server_endpoint);
    EXPECT_TRUE(allocator->BindSharedCollection(std::move(bind_shared_request)).is_ok());

    fidl::SyncClient collection_client{std::move(collection_client_endpoint)};
    SetDefaultCollectionNameV2(collection_client, fbl::StringPrintf("%u", i));
    if (i < client_count - 1) {
      // Ensure next_token is usable.
      EXPECT_TRUE(collection_client->Sync().is_ok());
    }
    result.emplace_back(std::move(collection_client));
  }
  return result;
}

fidl::SyncClient<v2::BufferCollectionToken> create_token_under_token_v2(
    fidl::SyncClient<v2::BufferCollectionToken>& token_a) {
  zx::result token_endpoints = fidl::CreateEndpoints<v2::BufferCollectionToken>();
  EXPECT_TRUE(token_endpoints.is_ok());
  auto& [token_b_client, token_b_server] = token_endpoints.value();
  v2::BufferCollectionTokenDuplicateRequest duplicate_request;
  duplicate_request.rights_attenuation_mask() = ZX_RIGHT_SAME_RIGHTS;
  duplicate_request.token_request() = std::move(token_b_server);
  EXPECT_TRUE(token_a->Duplicate(std::move(duplicate_request)).is_ok());
  fidl::SyncClient token_b{std::move(token_b_client)};
  EXPECT_TRUE(token_b->Sync().is_ok());
  return token_b;
}

fidl::SyncClient<v2::BufferCollectionTokenGroup> create_group_under_token_v2(
    fidl::SyncClient<v2::BufferCollectionToken>& token) {
  zx::result group_endpoints = fidl::CreateEndpoints<v2::BufferCollectionTokenGroup>();
  EXPECT_TRUE(group_endpoints.is_ok());
  auto& [group_client, group_server] = group_endpoints.value();
  v2::BufferCollectionTokenCreateBufferCollectionTokenGroupRequest create_group_request;
  create_group_request.group_request() = std::move(group_server);
  EXPECT_TRUE(token->CreateBufferCollectionTokenGroup(std::move(create_group_request)).is_ok());
  auto group = fidl::SyncClient(std::move(group_client));
  EXPECT_TRUE(group->Sync().is_ok());
  return group;
}

fidl::SyncClient<v2::BufferCollectionToken> create_token_under_group_v2(
    fidl::SyncClient<v2::BufferCollectionTokenGroup>& group,
    uint32_t rights_attenuation_mask = ZX_RIGHT_SAME_RIGHTS) {
  zx::result token_endpoints = fidl::CreateEndpoints<v2::BufferCollectionToken>();
  EXPECT_OK(token_endpoints.status_value());
  auto& [token_client, token_server] = token_endpoints.value();
  v2::BufferCollectionTokenGroupCreateChildRequest create_child_request;
  create_child_request.token_request() = std::move(token_server);
  if (rights_attenuation_mask != ZX_RIGHT_SAME_RIGHTS) {
    create_child_request.rights_attenuation_mask() = rights_attenuation_mask;
  }
  EXPECT_TRUE(group->CreateChild(std::move(create_child_request)).is_ok());
  auto token = fidl::SyncClient(std::move(token_client));
  EXPECT_TRUE(token->Sync().is_ok());
  return token;
}

void check_token_alive_v2(fidl::SyncClient<v2::BufferCollectionToken>& token) {
  constexpr uint32_t kIterations = 1;
  for (uint32_t i = 0; i < kIterations; ++i) {
    zx::nanosleep(zx::deadline_after(zx::usec(500)));
    EXPECT_TRUE(token->Sync().is_ok());
  }
}

void check_group_alive_v2(fidl::SyncClient<v2::BufferCollectionTokenGroup>& group) {
  constexpr uint32_t kIterations = 1;
  for (uint32_t i = 0; i < kIterations; ++i) {
    zx::nanosleep(zx::deadline_after(zx::usec(500)));
    EXPECT_TRUE(group->Sync().is_ok());
  }
}

fidl::SyncClient<v2::BufferCollection> convert_token_to_collection_v2(
    fidl::SyncClient<v2::BufferCollectionToken> token) {
  auto allocator = connect_to_sysmem_service_v2();
  zx::result collection_endpoints = fidl::CreateEndpoints<v2::BufferCollection>();
  EXPECT_TRUE(collection_endpoints.is_ok());
  auto& [collection_client, collection_server] = collection_endpoints.value();
  v2::AllocatorBindSharedCollectionRequest bind_shared_request;
  bind_shared_request.token() = token.TakeClientEnd();
  bind_shared_request.buffer_collection_request() = std::move(collection_server);
  EXPECT_TRUE(allocator->BindSharedCollection(std::move(bind_shared_request)).is_ok());
  return fidl::SyncClient(std::move(collection_client));
}

void set_picky_constraints_v2(fidl::SyncClient<v2::BufferCollection>& collection,
                              uint32_t exact_buffer_size) {
  EXPECT_EQ(exact_buffer_size % zx_system_get_page_size(), 0);
  v2::BufferCollectionConstraints constraints;
  constraints.usage().emplace();
  constraints.usage()->cpu() = v2::kCpuUsageReadOften | v2::kCpuUsageWriteOften;
  constraints.min_buffer_count_for_camping() = 1;
  constraints.buffer_memory_constraints().emplace();
  auto& buffer_memory = constraints.buffer_memory_constraints().value();
  buffer_memory.min_size_bytes() = exact_buffer_size;
  // Allow a max that's just large enough to accommodate the size implied
  // by the min frame size and PixelFormat.
  buffer_memory.max_size_bytes() = exact_buffer_size;
  buffer_memory.physically_contiguous_required() = false;
  buffer_memory.secure_required() = false;
  buffer_memory.ram_domain_supported() = false;
  buffer_memory.cpu_domain_supported() = true;
  buffer_memory.inaccessible_domain_supported() = false;
  ZX_DEBUG_ASSERT(!buffer_memory.heap_permitted().has_value());
  ZX_DEBUG_ASSERT(!constraints.image_format_constraints().has_value());
  v2::BufferCollectionSetConstraintsRequest set_constraints_request;
  set_constraints_request.constraints() = std::move(constraints);
  EXPECT_TRUE(collection->SetConstraints(std::move(set_constraints_request)).is_ok());
}

void set_min_camping_constraints_v2(fidl::SyncClient<v2::BufferCollection>& collection,
                                    uint32_t min_buffer_count_for_camping,
                                    uint32_t max_buffer_count = 0) {
  v2::BufferCollectionConstraints constraints;
  constraints.usage().emplace();
  constraints.usage()->cpu() = v2::kCpuUsageReadOften | v2::kCpuUsageWriteOften;
  constraints.min_buffer_count_for_camping() = min_buffer_count_for_camping;
  constraints.buffer_memory_constraints().emplace();
  auto& buffer_memory = constraints.buffer_memory_constraints().value();
  buffer_memory.min_size_bytes() = zx_system_get_page_size();
  // Allow a max that's just large enough to accommodate the size implied
  // by the min frame size and PixelFormat.
  buffer_memory.max_size_bytes() = zx_system_get_page_size();
  buffer_memory.physically_contiguous_required() = false;
  buffer_memory.secure_required() = false;
  buffer_memory.ram_domain_supported() = false;
  buffer_memory.cpu_domain_supported() = true;
  buffer_memory.inaccessible_domain_supported() = false;
  ZX_DEBUG_ASSERT(!buffer_memory.heap_permitted().has_value());
  if (max_buffer_count) {
    constraints.max_buffer_count() = max_buffer_count;
  }
  ZX_DEBUG_ASSERT(!constraints.image_format_constraints().has_value());
  v2::BufferCollectionSetConstraintsRequest set_constraints_request;
  set_constraints_request.constraints() = std::move(constraints);
  EXPECT_TRUE(collection->SetConstraints(std::move(set_constraints_request)).is_ok());
}

bool Equal(const v2::BufferCollectionInfo& lhs, const v2::BufferCollectionInfo& rhs) {
  // Clone both.
  auto clone = [](const v2::BufferCollectionInfo& v) -> v2::BufferCollectionInfo {
    auto clone_result = sysmem::V2CloneBufferCollectionInfo(v, 0, 0);
    ZX_ASSERT(clone_result.is_ok());
    return clone_result.take_value();
  };

  auto clone_lhs = clone(lhs);
  auto clone_rhs = clone(rhs);

  // Encode both.

  auto encoded_lhs = fidl::StandaloneEncode(std::move(clone_lhs));
  ZX_ASSERT_MSG(encoded_lhs.message().status() == ZX_OK, "encoded_lhs.message().status(): %d - %s",
                encoded_lhs.message().status(), encoded_lhs.message().FormatDescription().c_str());
  ZX_ASSERT(encoded_lhs.message().ok());
  ZX_DEBUG_ASSERT(encoded_lhs.message().handle_actual() == 0);

  auto encoded_rhs = fidl::StandaloneEncode(std::move(clone_rhs));
  ZX_ASSERT_MSG(encoded_rhs.message().status() == ZX_OK, "encoded_rhs.message().status(): %d - %s",
                encoded_rhs.message().status(), encoded_rhs.message().FormatDescription().c_str());
  ZX_ASSERT(encoded_rhs.message().ok());
  ZX_DEBUG_ASSERT(encoded_rhs.message().handle_actual() == 0);

  // Compare.
  return encoded_lhs.message().BytesMatch(encoded_rhs.message());
}

// Some helpers to test equality of buffer collection infos and related types.
template <typename T, size_t S>
bool ArrayEqual(const ::fidl::Array<T, S>& a, const ::fidl::Array<T, S>& b) {
  for (size_t i = 0; i < S; i++) {
    if (!Equal(a[i], b[i])) {
      return false;
    }
  }
  return true;
}

bool AttachTokenSucceedsV2(
    bool attach_before_also, bool fail_attached_early,
    fit::function<void(v2::BufferCollectionConstraints& to_modify)> modify_constraints_initiator,
    fit::function<void(v2::BufferCollectionConstraints& to_modify)> modify_constraints_participant,
    fit::function<void(v2::BufferCollectionConstraints& to_modify)> modify_constraints_attached,
    fit::function<void(v2::BufferCollectionInfo& to_verify)> verify_info,
    uint32_t expected_buffer_count = 6) {
  ZX_DEBUG_ASSERT(!fail_attached_early || attach_before_also);
  auto allocator = connect_to_sysmem_driver_v2();
  EXPECT_TRUE(allocator.is_ok());
  IF_FAILURES_RETURN_FALSE();

  zx::result token_endpoints_1 = fidl::CreateEndpoints<v2::BufferCollectionToken>();
  EXPECT_TRUE(token_endpoints_1.is_ok());
  IF_FAILURES_RETURN_FALSE();
  auto [token_client_1, token_server_1] = std::move(*token_endpoints_1);

  // Client 1 creates a token and new LogicalBufferCollection using
  // AllocateSharedCollection().
  v2::AllocatorAllocateSharedCollectionRequest allocate_shared_request;
  allocate_shared_request.token_request() = std::move(token_server_1);
  EXPECT_TRUE(allocator->AllocateSharedCollection(std::move(allocate_shared_request)).is_ok());
  IF_FAILURES_RETURN_FALSE();

  zx::result token_endpoints_2 = fidl::CreateEndpoints<v2::BufferCollectionToken>();
  EXPECT_TRUE(token_endpoints_2.is_ok());
  IF_FAILURES_RETURN_FALSE();
  auto [token_client_2, token_server_2] = std::move(*token_endpoints_2);

  // Client 1 duplicates its token and gives the duplicate to client 2 (this
  // test is single proc, so both clients are coming from this client
  // process - normally the two clients would be in separate processes with
  // token_client_2 transferred to another participant).
  fidl::SyncClient token_1{std::move(token_client_1)};
  v2::BufferCollectionTokenDuplicateRequest duplicate_request;
  duplicate_request.rights_attenuation_mask() = ZX_RIGHT_SAME_RIGHTS;
  duplicate_request.token_request() = std::move(token_server_2);
  EXPECT_TRUE(token_1->Duplicate(std::move(duplicate_request)).is_ok());
  IF_FAILURES_RETURN_FALSE();

  // Client 3 is attached later.

  zx::result collection_endpoints_1 = fidl::CreateEndpoints<v2::BufferCollection>();
  EXPECT_TRUE(collection_endpoints_1.is_ok());
  IF_FAILURES_RETURN_FALSE();
  auto [collection_client_1, collection_server_1] = std::move(*collection_endpoints_1);
  fidl::SyncClient collection_1{std::move(collection_client_1)};

  EXPECT_NE(token_1.client_end().channel().get(), ZX_HANDLE_INVALID);

  v2::AllocatorBindSharedCollectionRequest bind_shared_request;
  bind_shared_request.token() = token_1.TakeClientEnd();
  bind_shared_request.buffer_collection_request() = std::move(collection_server_1);
  EXPECT_TRUE(allocator->BindSharedCollection(std::move(bind_shared_request)).is_ok());
  IF_FAILURES_RETURN_FALSE();

  v2::BufferCollectionConstraints constraints_1;
  constraints_1.usage().emplace();
  constraints_1.usage()->cpu() = v2::kCpuUsageReadOften | v2::kCpuUsageWriteOften;
  constraints_1.min_buffer_count_for_camping() = 3;
  constraints_1.buffer_memory_constraints().emplace();
  auto& buffer_memory = constraints_1.buffer_memory_constraints().value();
  // This min_size_bytes is intentionally too small to hold the min_coded_width and
  // min_coded_height in NV12
  // format.
  buffer_memory.min_size_bytes() = 64 * 1024;
  // Allow a max that's just large enough to accommodate the size implied
  // by the min frame size and PixelFormat.
  buffer_memory.max_size_bytes() = (512 * 512) * 3 / 2;
  buffer_memory.physically_contiguous_required() = false;
  buffer_memory.secure_required() = false;
  buffer_memory.ram_domain_supported() = false;
  buffer_memory.cpu_domain_supported() = true;
  buffer_memory.inaccessible_domain_supported() = false;
  ZX_DEBUG_ASSERT(!buffer_memory.heap_permitted().has_value());
  constraints_1.image_format_constraints().emplace(1);
  auto& image_constraints_1 = constraints_1.image_format_constraints()->at(0);
  image_constraints_1.pixel_format() = fuchsia_images2::PixelFormat::kNv12;
  image_constraints_1.color_spaces().emplace(1);
  image_constraints_1.color_spaces()->at(0) = fuchsia_images2::ColorSpace::kRec709;
  // The min dimensions intentionally imply a min size that's larger than
  // buffer_memory_constraints.min_size_bytes.
  image_constraints_1.min_size() = {256, 256};
  image_constraints_1.max_size() = {std::numeric_limits<uint32_t>::max(),
                                    std::numeric_limits<uint32_t>::max()};
  image_constraints_1.min_bytes_per_row() = 256;
  image_constraints_1.max_bytes_per_row() = std::numeric_limits<uint32_t>::max();
  image_constraints_1.max_surface_width_times_surface_height() =
      std::numeric_limits<uint32_t>::max();
  image_constraints_1.size_alignment() = {2, 2};
  image_constraints_1.bytes_per_row_divisor() = 2;
  image_constraints_1.start_offset_divisor() = 2;
  image_constraints_1.display_rect_alignment() = {1, 1};

  // Start with constraints_2 a copy of constraints_1.  There are no handles
  // in the constraints struct so a struct copy instead of clone is fine here.
  v2::BufferCollectionConstraints constraints_2(constraints_1);
  // Modify constraints_2 to require double the width and height.
  constraints_2.image_format_constraints()->at(0).min_size() = {512, 512};

  // TODO(fxb/115937): Fix this to work for sysmem2.
#if SYSMEM_FUZZ_CORPUS
  FILE* ofp = fopen("/cache/sysmem_fuzz_corpus_multi_buffer_collecton_constraints.dat", "wb");
  if (ofp) {
    fwrite(&constraints_1, sizeof(fuchsia_sysmem::wire::BufferCollectionConstraints), 1, ofp);
    fwrite(&constraints_2, sizeof(fuchsia_sysmem::wire::BufferCollectionConstraints), 1, ofp);
    fclose(ofp);
  } else {
    printf("Failed to write sysmem multi BufferCollectionConstraints corpus file.\n");
    fflush(stderr);
  }
#endif  // SYSMEM_FUZZ_CORPUS

  modify_constraints_initiator(constraints_1);

  v2::BufferCollectionSetConstraintsRequest set_constraints_request;
  set_constraints_request.constraints() = std::move(constraints_1);
  EXPECT_TRUE(collection_1->SetConstraints(std::move(set_constraints_request)).is_ok());
  IF_FAILURES_RETURN_FALSE();

  // Client 2 connects to sysmem separately.
  auto allocator_2 = connect_to_sysmem_driver_v2();
  EXPECT_TRUE(allocator_2.is_ok());
  IF_FAILURES_RETURN_FALSE();

  zx::result collection_endpoints_2 = fidl::CreateEndpoints<v2::BufferCollection>();
  EXPECT_TRUE(collection_endpoints_2.is_ok());
  IF_FAILURES_RETURN_FALSE();
  auto [collection_client_2, collection_server_2] = std::move(*collection_endpoints_2);
  fidl::SyncClient collection_2{std::move(collection_client_2)};

  // Just because we can, perform this sync as late as possible, just before
  // the BindSharedCollection() via allocator2_client_2.  Without this Sync(),
  // the BindSharedCollection() might arrive at the server before the
  // Duplicate() that delivered the server end of token_client_2 to sysmem,
  // which would cause sysmem to not recognize the token.
  EXPECT_TRUE(collection_1->Sync().is_ok());
  IF_FAILURES_RETURN_FALSE();

  EXPECT_NE(token_client_2.channel().get(), ZX_HANDLE_INVALID);

  v2::AllocatorBindSharedCollectionRequest bind_shared_request2;
  bind_shared_request2.token() = std::move(token_client_2);
  bind_shared_request2.buffer_collection_request() = std::move(collection_server_2);
  EXPECT_TRUE(allocator_2->BindSharedCollection(std::move(bind_shared_request2)).is_ok());
  IF_FAILURES_RETURN_FALSE();

  // Not all constraints have been input, so the buffers haven't been
  // allocated yet.
  auto check_1_result = collection_1->CheckAllBuffersAllocated();
  EXPECT_FALSE(check_1_result.is_ok());
  EXPECT_TRUE(check_1_result.error_value().is_domain_error());
  EXPECT_EQ(check_1_result.error_value().domain_error(), ZX_ERR_UNAVAILABLE);
  IF_FAILURES_RETURN_FALSE();

  auto check_2_result = collection_2->CheckAllBuffersAllocated();
  EXPECT_FALSE(check_2_result.is_ok());
  EXPECT_TRUE(check_2_result.error_value().is_domain_error());
  EXPECT_EQ(check_2_result.error_value().domain_error(), ZX_ERR_UNAVAILABLE);
  IF_FAILURES_RETURN_FALSE();

  fidl::ClientEnd<v2::BufferCollectionToken> token_client_3;
  fidl::ServerEnd<v2::BufferCollectionToken> token_server_3;

  auto use_collection_2_to_attach_token_3 = [&collection_2, &token_client_3, &token_server_3] {
    token_client_3.reset();
    ZX_DEBUG_ASSERT(!token_server_3);
    zx::result token_endpoints_3 = fidl::CreateEndpoints<v2::BufferCollectionToken>();
    EXPECT_TRUE(token_endpoints_3.is_ok());
    IF_FAILURES_RETURN();
    token_client_3 = std::move(token_endpoints_3->client);
    token_server_3 = std::move(token_endpoints_3->server);

    v2::BufferCollectionAttachTokenRequest attach_request;
    attach_request.rights_attenuation_mask() = ZX_RIGHT_SAME_RIGHTS;
    attach_request.token_request() = std::move(token_server_3);
    EXPECT_TRUE(collection_2->AttachToken(std::move(attach_request)).is_ok());
    IF_FAILURES_RETURN();
    // Since we're not doing any Duplicate()s first or anything like that (which could allow us to
    // share the round trip), go ahead and Sync() the token creation to sysmem now.
    EXPECT_TRUE(collection_2->Sync().is_ok());
    IF_FAILURES_RETURN();
  };

  if (attach_before_also) {
    use_collection_2_to_attach_token_3();
  }
  IF_FAILURES_RETURN_FALSE();

  // The AttachToken() participant needs to set constraints also, but it will never hold up initial
  // allocation.

  v2::BufferCollectionConstraints constraints_3;
  constraints_3.usage().emplace();
  constraints_3.usage()->cpu() = v2::kCpuUsageReadOften | v2::kCpuUsageWriteOften;
  constraints_3.buffer_memory_constraints().emplace();
  constraints_3.buffer_memory_constraints()->cpu_domain_supported() = true;
  modify_constraints_attached(constraints_3);

  fidl::SyncClient<v2::BufferCollection> collection_3;

  auto collection_client_3_set_constraints = [&allocator_2, &token_client_3, &constraints_3,
                                              &collection_3] {
    EXPECT_NE(allocator_2.value().client_end().channel().get(), ZX_HANDLE_INVALID);
    EXPECT_NE(token_client_3.channel().get(), ZX_HANDLE_INVALID);
    IF_FAILURES_RETURN();
    collection_3 = {};
    ZX_DEBUG_ASSERT(!collection_3.is_valid());

    zx::result collection_endpoints_3 = fidl::CreateEndpoints<v2::BufferCollection>();
    EXPECT_TRUE(collection_endpoints_3.is_ok());
    IF_FAILURES_RETURN();
    auto [collection_client_3, collection_server_3] = std::move(*collection_endpoints_3);
    collection_3 = fidl::SyncClient(std::move(collection_client_3));

    v2::AllocatorBindSharedCollectionRequest bind_shared_request;
    bind_shared_request.token() = std::move(token_client_3);
    bind_shared_request.buffer_collection_request() = std::move(collection_server_3);
    EXPECT_TRUE(allocator_2->BindSharedCollection(std::move(bind_shared_request)).is_ok());
    IF_FAILURES_RETURN();
    v2::BufferCollectionConstraints constraints_3_copy(constraints_3);

    v2::BufferCollectionSetConstraintsRequest set_constraints_request;
    set_constraints_request.constraints() = std::move(constraints_3_copy);
    EXPECT_TRUE(collection_3->SetConstraints(std::move(set_constraints_request)).is_ok());
    IF_FAILURES_RETURN();
  };

  if (attach_before_also) {
    collection_client_3_set_constraints();
    IF_FAILURES_RETURN_FALSE();
    if (fail_attached_early) {
      // Also close the channel to simulate early client 3 failure before allocation.
      collection_3 = {};
    }
  }

  //
  // Only after all non-AttachToken() participants have SetConstraints() will the initial allocation
  // be successful.  The initial allocation will always succeed regardless of how uncooperative the
  // AttachToken() client 3 is being with its constraints.
  //

  modify_constraints_participant(constraints_2);
  v2::BufferCollectionSetConstraintsRequest set_constraints_request2;
  set_constraints_request2.constraints() = std::move(constraints_2);
  EXPECT_TRUE(collection_2->SetConstraints(std::move(set_constraints_request2)).is_ok());
  IF_FAILURES_RETURN_FALSE();

  auto allocate_result_1 = collection_1->WaitForAllBuffersAllocated();

  // This is the first round-trip to/from sysmem.  A failure here can be due
  // to any step above failing async.
  EXPECT_TRUE(allocate_result_1.is_ok());
  IF_FAILURES_RETURN_FALSE();

  auto check_result_good_1 = collection_1->CheckAllBuffersAllocated();
  EXPECT_TRUE(check_result_good_1.is_ok());
  IF_FAILURES_RETURN_FALSE();

  auto check_result_good_2 = collection_2->CheckAllBuffersAllocated();
  EXPECT_TRUE(check_result_good_2.is_ok());
  IF_FAILURES_RETURN_FALSE();

  auto allocate_result_2 = collection_2->WaitForAllBuffersAllocated();
  EXPECT_TRUE(allocate_result_2.is_ok());
  IF_FAILURES_RETURN_FALSE();

  //
  // buffer_collection_info_1 and buffer_collection_info_2 should be exactly
  // equal except their non-zero handle values, which should be different.  We
  // verify the handle values then check that the structs are exactly the same
  // with handle values zeroed out.
  //
  v2::BufferCollectionInfo& buffer_collection_info_1 =
      allocate_result_1->buffer_collection_info().value();
  v2::BufferCollectionInfo& buffer_collection_info_2 =
      allocate_result_2->buffer_collection_info().value();
  v2::BufferCollectionInfo buffer_collection_info_3;

  EXPECT_EQ(buffer_collection_info_1.buffers()->size(), buffer_collection_info_2.buffers()->size());
  for (uint32_t i = 0; i < buffer_collection_info_1.buffers()->size(); ++i) {
    EXPECT_NE(buffer_collection_info_1.buffers()->at(i).vmo()->get(), ZX_HANDLE_INVALID);
    EXPECT_NE(buffer_collection_info_2.buffers()->at(i).vmo()->get(), ZX_HANDLE_INVALID);
    IF_FAILURES_RETURN_FALSE();
    // The handle values must be different.
    EXPECT_NE(buffer_collection_info_1.buffers()->at(i).vmo()->get(),
              buffer_collection_info_2.buffers()->at(i).vmo()->get());
    IF_FAILURES_RETURN_FALSE();
    // For now, the koid(s) are expected to be equal.  This is not a
    // fundamental check, in that sysmem could legitimately change in
    // future to vend separate child VMOs (of the same portion of a
    // non-copy-on-write parent VMO) to the two participants and that
    // would still be potentially valid overall.
    zx_koid_t koid_1 = get_koid(buffer_collection_info_1.buffers()->at(i).vmo()->get());
    zx_koid_t koid_2 = get_koid(buffer_collection_info_2.buffers()->at(i).vmo()->get());
    EXPECT_EQ(koid_1, koid_2);
    IF_FAILURES_RETURN_FALSE();
  }

  //
  // Verify that buffer_collection_info_1 paid attention to constraints_2, and
  // that buffer_collection_info_2 makes sense.  This also indirectly confirms
  // that buffer_collection_info_3 paid attention to constraints_2.
  //

  EXPECT_EQ(buffer_collection_info_1.buffers()->size(), expected_buffer_count);
  // The size should be sufficient for the whole NV12 frame, not just
  // min_size_bytes.  In other words, the portion of the VMO the client can
  // use is large enough to hold the min image size, despite the min buffer
  // size being smaller.
  EXPECT_GE(buffer_collection_info_1.settings()->buffer_settings()->size_bytes().value(),
            (512 * 512) * 3 / 2);
  EXPECT_FALSE(
      buffer_collection_info_1.settings()->buffer_settings()->is_physically_contiguous().value());
  EXPECT_FALSE(buffer_collection_info_1.settings()->buffer_settings()->is_secure().value());
  // We specified image_format_constraints so the result must also have
  // image_format_constraints.
  EXPECT_TRUE(buffer_collection_info_1.settings()->image_format_constraints().has_value());
  IF_FAILURES_RETURN_FALSE();

  for (uint32_t i = 0; i < buffer_collection_info_1.buffers()->size(); ++i) {
    EXPECT_NE(buffer_collection_info_1.buffers()->at(i).vmo()->get(), ZX_HANDLE_INVALID);
    EXPECT_NE(buffer_collection_info_2.buffers()->at(i).vmo()->get(), ZX_HANDLE_INVALID);
    IF_FAILURES_RETURN_FALSE();

    uint64_t size_bytes_1 = 0;
    EXPECT_OK(buffer_collection_info_1.buffers()->at(i).vmo()->get_size(&size_bytes_1));
    IF_FAILURES_RETURN_FALSE();

    uint64_t size_bytes_2 = 0;
    EXPECT_OK(buffer_collection_info_2.buffers()->at(i).vmo()->get_size(&size_bytes_2));
    IF_FAILURES_RETURN_FALSE();

    // The vmo has room for the nominal size of the portion of the VMO
    // the client can use.  These checks should pass even if sysmem were
    // to vend different child VMOs to the two participants.
    EXPECT_LE(buffer_collection_info_1.buffers()->at(i).vmo_usable_start().value() +
                  buffer_collection_info_1.settings()->buffer_settings()->size_bytes().value(),
              size_bytes_1);
    EXPECT_LE(buffer_collection_info_2.buffers()->at(i).vmo_usable_start().value() +
                  buffer_collection_info_2.settings()->buffer_settings()->size_bytes().value(),
              size_bytes_2);
    IF_FAILURES_RETURN_FALSE();
  }

  if (attach_before_also && !collection_3.is_valid()) {
    // We already failed collection_client_3 early, so AttachToken() can't succeed, but we've
    // checked that initial allocation did succeed despite the pre-allocation
    // failure of client 3.
    return false;
  }

  if (attach_before_also) {
    auto allocate_result_3 = collection_3->WaitForAllBuffersAllocated();
    if (!allocate_result_3.is_ok()) {
      return false;
    }
  }

  const uint32_t kIterationCount = 3;
  for (uint32_t i = 0; i < kIterationCount; ++i) {
    if (i != 0 || !attach_before_also) {
      use_collection_2_to_attach_token_3();
      collection_client_3_set_constraints();
    }

    // The collection_client_3_set_constraints() above closed the old collection_client_3, which the
    // sysmem server treats as a client 3 failure, but because client 3 was created via
    // AttachToken(), the failure of client 3 doesn't cause failure of the LogicalBufferCollection.
    //
    // Give some time to fail if it were going to (but it shouldn't).
    nanosleep_duration(zx::msec(250));
    EXPECT_TRUE(collection_1->Sync().is_ok());
    // LogicalBufferCollection still ok.
    IF_FAILURES_RETURN_FALSE();

    auto allocate_result_3 = collection_3->WaitForAllBuffersAllocated();
    if (!allocate_result_3.is_ok()) {
      return false;
    }

    buffer_collection_info_3 = std::move(allocate_result_3->buffer_collection_info().value());
    allocate_result_3->buffer_collection_info().reset();
    EXPECT_EQ(buffer_collection_info_3.buffers()->size(),
              buffer_collection_info_1.buffers()->size());

    for (uint32_t i = 0; i < buffer_collection_info_1.buffers()->size(); ++i) {
      EXPECT_EQ(buffer_collection_info_1.buffers()->at(i).vmo()->get() != ZX_HANDLE_INVALID,
                buffer_collection_info_3.buffers()->at(i).vmo()->get() != ZX_HANDLE_INVALID);
      IF_FAILURES_RETURN_FALSE();
      if (buffer_collection_info_1.buffers()->at(i).vmo()->get() != ZX_HANDLE_INVALID) {
        // The handle values must be different.
        EXPECT_NE(buffer_collection_info_1.buffers()->at(i).vmo()->get(),
                  buffer_collection_info_3.buffers()->at(i).vmo()->get());
        EXPECT_NE(buffer_collection_info_2.buffers()->at(i).vmo()->get(),
                  buffer_collection_info_3.buffers()->at(i).vmo()->get());
        IF_FAILURES_RETURN_FALSE();
        // For now, the koid(s) are expected to be equal.  This is not a
        // fundamental check, in that sysmem could legitimately change in
        // future to vend separate child VMOs (of the same portion of a
        // non-copy-on-write parent VMO) to the two participants and that
        // would still be potentially valid overall.
        zx_koid_t koid_1 = get_koid(buffer_collection_info_1.buffers()->at(i).vmo()->get());
        zx_koid_t koid_3 = get_koid(buffer_collection_info_3.buffers()->at(i).vmo()->get());
        EXPECT_EQ(koid_1, koid_3);
        IF_FAILURES_RETURN_FALSE();
      }
    }
  }

  // Clear out vmo handles and compare all three collections
  for (uint32_t i = 0; i < buffer_collection_info_1.buffers()->size(); ++i) {
    buffer_collection_info_1.buffers()->at(i).vmo().reset();
    buffer_collection_info_2.buffers()->at(i).vmo().reset();
    buffer_collection_info_3.buffers()->at(i).vmo().reset();
  }

  // Check that buffer_collection_info_1 and buffer_collection_info_2 are
  // consistent.
  EXPECT_TRUE(Equal(buffer_collection_info_1, buffer_collection_info_2));
  IF_FAILURES_RETURN_FALSE();

  // Check that buffer_collection_info_1 and buffer_collection_info_3 are
  // consistent.
  EXPECT_TRUE(Equal(buffer_collection_info_1, buffer_collection_info_3));
  IF_FAILURES_RETURN_FALSE();

  return true;
}

}  // namespace

TEST(Sysmem, DriverConnectionV2) {
  auto allocator = connect_to_sysmem_driver_v2();
  ASSERT_TRUE(allocator.is_ok());
  ASSERT_OK(verify_connectivity_v2(allocator.value()));
}

TEST(Sysmem, ServiceConnectionV2) {
  auto allocator = connect_to_sysmem_service_v2();
  ASSERT_OK(allocator);
  ASSERT_OK(verify_connectivity_v2(allocator.value()));
}

TEST(Sysmem, VerifyBufferCollectionTokenV2) {
  auto allocator = connect_to_sysmem_driver_v2();
  ASSERT_TRUE(allocator.is_ok());

  auto token_endpoints = fidl::CreateEndpoints<fuchsia_sysmem2::BufferCollectionToken>();
  ASSERT_TRUE(token_endpoints.is_ok());
  auto [token_client, token_server] = std::move(*token_endpoints);
  fidl::SyncClient token{std::move(token_client)};

  fuchsia_sysmem2::AllocatorAllocateSharedCollectionRequest request;
  request.token_request() = std::move(token_server);
  ASSERT_TRUE(allocator->AllocateSharedCollection(std::move(request)).is_ok());

  auto token2_endpoints = fidl::CreateEndpoints<fuchsia_sysmem2::BufferCollectionToken>();
  ASSERT_TRUE(token2_endpoints.is_ok());
  auto [token2_client, token2_server] = std::move(*token2_endpoints);
  fidl::SyncClient token2{std::move(token2_client)};

  fuchsia_sysmem2::BufferCollectionTokenDuplicateRequest duplicate_request;
  duplicate_request.rights_attenuation_mask() = ZX_RIGHT_SAME_RIGHTS;
  duplicate_request.token_request() = std::move(token2_server);
  ASSERT_TRUE(token->Duplicate(std::move(duplicate_request)).is_ok());

  auto not_token_endpoints = fidl::CreateEndpoints<fuchsia_sysmem2::BufferCollectionToken>();
  ASSERT_TRUE(not_token_endpoints.is_ok());
  auto [not_token_client, not_token_server] = std::move(*not_token_endpoints);

  ASSERT_TRUE(token->Sync().is_ok());
  ASSERT_TRUE(token2->Sync().is_ok());

  fuchsia_sysmem2::AllocatorValidateBufferCollectionTokenRequest validate_request;
  validate_request.token_server_koid() = get_related_koid(token.client_end().channel().get());
  auto validate_result_1 = allocator->ValidateBufferCollectionToken(std::move(validate_request));
  ASSERT_TRUE(validate_result_1.is_ok());
  ASSERT_TRUE(validate_result_1->is_known().has_value());
  ASSERT_TRUE(validate_result_1->is_known().value());

  fuchsia_sysmem2::AllocatorValidateBufferCollectionTokenRequest validate_request2;
  validate_request2.token_server_koid() = get_related_koid(token2.client_end().channel().get());
  auto validate_result_2 = allocator->ValidateBufferCollectionToken(std::move(validate_request2));
  ASSERT_TRUE(validate_result_2.is_ok());
  ASSERT_TRUE(validate_result_2->is_known().has_value());
  ASSERT_TRUE(validate_result_2->is_known().value());

  fuchsia_sysmem2::AllocatorValidateBufferCollectionTokenRequest validate_request3;
  validate_request3.token_server_koid() = get_related_koid(not_token_client.channel().get());
  auto validate_result_not_known =
      allocator->ValidateBufferCollectionToken(std::move(validate_request3));
  ASSERT_TRUE(validate_result_not_known.is_ok());
  ASSERT_TRUE(validate_result_not_known->is_known().has_value());
  ASSERT_FALSE(validate_result_not_known->is_known().value());
}

TEST(Sysmem, TokenOneParticipantNoImageConstraintsV2) {
  auto collection = make_single_participant_collection_v2();
  ASSERT_TRUE(collection.is_ok());

  fuchsia_sysmem2::BufferCollectionConstraints constraints;
  constraints.usage().emplace();
  constraints.usage()->cpu() =
      fuchsia_sysmem::wire::kCpuUsageReadOften | fuchsia_sysmem::wire::kCpuUsageWriteOften;
  constraints.min_buffer_count_for_camping() = 3;
  constraints.buffer_memory_constraints().emplace();
  fuchsia_sysmem2::BufferMemoryConstraints& buffer_memory =
      constraints.buffer_memory_constraints().value();
  buffer_memory.min_size_bytes() = 64 * 1024;
  buffer_memory.max_size_bytes() = 128 * 1024;
  buffer_memory.physically_contiguous_required() = false;
  buffer_memory.secure_required() = false;
  buffer_memory.ram_domain_supported() = false;
  buffer_memory.cpu_domain_supported() = true;
  buffer_memory.inaccessible_domain_supported() = false;
  ZX_DEBUG_ASSERT(!buffer_memory.heap_permitted().has_value());
  ZX_DEBUG_ASSERT(!constraints.image_format_constraints().has_value());

  fuchsia_sysmem2::BufferCollectionSetConstraintsRequest request;
  request.constraints() = std::move(constraints);
  ASSERT_TRUE(collection->SetConstraints(std::move(request)).is_ok());

  auto allocation_result = collection->WaitForAllBuffersAllocated();
  // This is the first round-trip to/from sysmem.  A failure here can be due
  // to any step above failing async.
  ASSERT_TRUE(allocation_result.is_ok());

  auto& buffer_collection_info = allocation_result->buffer_collection_info().value();
  ASSERT_TRUE(buffer_collection_info.buffers().has_value());
  ASSERT_EQ(buffer_collection_info.buffers()->size(), 3);
  ASSERT_TRUE(buffer_collection_info.settings().has_value());
  ASSERT_TRUE(buffer_collection_info.settings()->buffer_settings().has_value());
  ASSERT_EQ(buffer_collection_info.settings()->buffer_settings()->size_bytes().value(), 64 * 1024);
  ASSERT_EQ(
      buffer_collection_info.settings()->buffer_settings()->is_physically_contiguous().value(),
      false);
  ASSERT_EQ(buffer_collection_info.settings()->buffer_settings()->is_secure().value(), false);
  ASSERT_EQ(buffer_collection_info.settings()->buffer_settings()->coherency_domain().value(),
            fuchsia_sysmem2::CoherencyDomain::kCpu);
  ASSERT_FALSE(buffer_collection_info.settings()->image_format_constraints().has_value());

  for (uint32_t i = 0; i < buffer_collection_info.buffers()->size(); ++i) {
    ASSERT_TRUE(buffer_collection_info.buffers()->at(i).vmo().has_value());
    ASSERT_NE(buffer_collection_info.buffers()->at(i).vmo()->get(), ZX_HANDLE_INVALID);
    uint64_t size_bytes = 0;
    auto status =
        zx_vmo_get_size(buffer_collection_info.buffers()->at(i).vmo()->get(), &size_bytes);
    ASSERT_OK(status);
    ASSERT_EQ(size_bytes, 64 * 1024);
  }
}

TEST(Sysmem, TokenOneParticipantColorspaceRankingV2) {
  auto collection = make_single_participant_collection_v2();
  ASSERT_TRUE(collection.is_ok());

  fuchsia_sysmem2::BufferCollectionConstraints constraints;
  constraints.usage().emplace();
  constraints.usage()->cpu() =
      fuchsia_sysmem::wire::kCpuUsageReadOften | fuchsia_sysmem::wire::kCpuUsageWriteOften;
  constraints.min_buffer_count_for_camping() = 1;
  constraints.buffer_memory_constraints().emplace();
  auto& buffer_memory = constraints.buffer_memory_constraints().value();
  buffer_memory.min_size_bytes() = 64 * 1024;
  buffer_memory.max_size_bytes() = 128 * 1024;
  buffer_memory.cpu_domain_supported() = true;
  constraints.image_format_constraints().emplace(1);
  auto& image_constraints = constraints.image_format_constraints()->at(0);
  image_constraints.pixel_format() = fuchsia_images2::PixelFormat::kNv12;
  image_constraints.color_spaces().emplace(3);
  image_constraints.color_spaces()->at(0) = fuchsia_images2::ColorSpace::kRec601Pal;
  image_constraints.color_spaces()->at(1) = fuchsia_images2::ColorSpace::kRec601PalFullRange;
  image_constraints.color_spaces()->at(2) = fuchsia_images2::ColorSpace::kRec709;

  fuchsia_sysmem2::BufferCollectionSetConstraintsRequest request;
  request.constraints() = std::move(constraints);
  ASSERT_TRUE(collection->SetConstraints(std::move(request)).is_ok());

  auto allocation_result = collection->WaitForAllBuffersAllocated();
  ASSERT_TRUE(allocation_result.is_ok());

  auto& buffer_collection_info = allocation_result->buffer_collection_info().value();
  ASSERT_TRUE(buffer_collection_info.buffers().has_value());
  ASSERT_EQ(buffer_collection_info.buffers()->size(), 1);
  ASSERT_TRUE(buffer_collection_info.settings().has_value());
  ASSERT_TRUE(buffer_collection_info.settings()->image_format_constraints().has_value());
  ASSERT_TRUE(
      buffer_collection_info.settings()->image_format_constraints()->pixel_format().has_value());
  ASSERT_EQ(buffer_collection_info.settings()->image_format_constraints()->pixel_format().value(),
            fuchsia_images2::PixelFormat::kNv12);
  ASSERT_TRUE(
      buffer_collection_info.settings()->image_format_constraints()->color_spaces().has_value());
  ASSERT_EQ(buffer_collection_info.settings()->image_format_constraints()->color_spaces()->size(),
            1);
  ASSERT_EQ(buffer_collection_info.settings()->image_format_constraints()->color_spaces()->at(0),
            fuchsia_images2::ColorSpace::kRec709);
}

TEST(Sysmem, AttachLifetimeTrackingV2) {
  auto collection = make_single_participant_collection_v2();
  ASSERT_TRUE(collection.is_ok());

  ASSERT_TRUE(collection->Sync().is_ok());

  constexpr uint32_t kNumBuffers = 3;
  constexpr uint32_t kNumEventpairs = kNumBuffers + 3;
  zx::eventpair client[kNumEventpairs];
  zx::eventpair server[kNumEventpairs];
  for (uint32_t i = 0; i < kNumEventpairs; ++i) {
    ASSERT_OK(zx::eventpair::create(/*options=*/0, &client[i], &server[i]));
    fuchsia_sysmem2::BufferCollectionAttachLifetimeTrackingRequest request;
    request.server_end() = std::move(server[i]);
    request.buffers_remaining() = i;
    ASSERT_TRUE(collection->AttachLifetimeTracking(std::move(request)).is_ok());
  }

  nanosleep_duration(zx::msec(500));

  for (uint32_t i = 0; i < kNumEventpairs; ++i) {
    zx_signals_t pending_signals;
    auto status =
        client[i].wait_one(ZX_EVENTPAIR_PEER_CLOSED, zx::time::infinite_past(), &pending_signals);
    ASSERT_EQ(status, ZX_ERR_TIMED_OUT);
    // Buffers are not allocated yet, so lifetime tracking is pending, since we don't yet know how
    // many buffers there will be.
    ASSERT_EQ(pending_signals & ZX_EVENTPAIR_PEER_CLOSED, 0);
  }

  fuchsia_sysmem2::BufferCollectionConstraints constraints;
  constraints.usage().emplace();
  constraints.usage()->cpu() =
      fuchsia_sysmem::wire::kCpuUsageReadOften | fuchsia_sysmem::wire::kCpuUsageWriteOften;
  constraints.min_buffer_count_for_camping() = kNumBuffers;
  constraints.buffer_memory_constraints().emplace();
  auto& buffer_memory = constraints.buffer_memory_constraints().value();
  buffer_memory.min_size_bytes() = 64 * 1024;
  buffer_memory.max_size_bytes() = 128 * 1024;
  buffer_memory.physically_contiguous_required() = false;
  buffer_memory.secure_required() = false;
  buffer_memory.ram_domain_supported() = false;
  buffer_memory.cpu_domain_supported() = true;
  buffer_memory.inaccessible_domain_supported() = false;
  ZX_DEBUG_ASSERT(!buffer_memory.heap_permitted().has_value());
  ZX_DEBUG_ASSERT(!constraints.image_format_constraints().has_value());
  fuchsia_sysmem2::BufferCollectionSetConstraintsRequest request;
  request.constraints() = std::move(constraints);
  auto set_constraints_result = collection->SetConstraints(std::move(request));
  ASSERT_TRUE(set_constraints_result.is_ok());

  // Enough time to typically notice if server accidentally closes server 0..kNumEventpairs-1.
  nanosleep_duration(zx::msec(200));

  // Now that we've set constraints, allocation can happen, and ZX_EVENTPAIR_PEER_CLOSED should be
  // seen for eventpair(s) >= kNumBuffers.
  for (uint32_t i = 0; i < kNumEventpairs; ++i) {
    zx_signals_t pending_signals;
    auto status = client[i].wait_one(
        ZX_EVENTPAIR_PEER_CLOSED,
        i >= kNumBuffers ? zx::time::infinite() : zx::time::infinite_past(), &pending_signals);
    ASSERT_TRUE(status == (i >= kNumBuffers) ? ZX_OK : ZX_ERR_TIMED_OUT);
    ASSERT_TRUE(!!(pending_signals & ZX_EVENTPAIR_PEER_CLOSED) == (i >= kNumBuffers));
  }

  auto wait_for_buffers_allocated_result = collection->WaitForAllBuffersAllocated();
  ASSERT_TRUE(wait_for_buffers_allocated_result.is_ok());
  auto& info = wait_for_buffers_allocated_result->buffer_collection_info().value();
  ASSERT_EQ(info.buffers()->size(), kNumBuffers);

  // ZX_EVENTPAIR_PEER_CLOSED should be seen for eventpair(s) >= kNumBuffers.
  for (uint32_t i = 0; i < kNumEventpairs; ++i) {
    zx_signals_t pending_signals;
    auto status = client[i].wait_one(
        ZX_EVENTPAIR_PEER_CLOSED,
        i >= kNumBuffers ? zx::time::infinite() : zx::time::infinite_past(), &pending_signals);
    ASSERT_TRUE(status == (i >= kNumBuffers) ? ZX_OK : ZX_ERR_TIMED_OUT);
    ASSERT_TRUE(!!(pending_signals & ZX_EVENTPAIR_PEER_CLOSED) == (i >= kNumBuffers));
  }

  zx::result attached_token_endpoints =
      fidl::CreateEndpoints<fuchsia_sysmem2::BufferCollectionToken>();
  ASSERT_TRUE(attached_token_endpoints.is_ok());
  auto [attached_token_client, attached_token_server] = std::move(*attached_token_endpoints);

  fuchsia_sysmem2::BufferCollectionAttachTokenRequest attach_request;
  attach_request.rights_attenuation_mask() = std::numeric_limits<uint32_t>::max();
  attach_request.token_request() = std::move(attached_token_server);
  ASSERT_TRUE(collection->AttachToken(std::move(attach_request)).is_ok());

  ASSERT_TRUE(collection->Sync().is_ok());

  zx::result attached_collection_endpoints =
      fidl::CreateEndpoints<fuchsia_sysmem2::BufferCollection>();
  ASSERT_TRUE(attached_collection_endpoints.is_ok());
  auto [attached_collection_client, attached_collection_server] =
      std::move(*attached_collection_endpoints);
  fidl::SyncClient attached_collection(std::move(attached_collection_client));

  auto allocator = connect_to_sysmem_driver_v2();
  ASSERT_TRUE(allocator.is_ok());
  fuchsia_sysmem2::AllocatorBindSharedCollectionRequest bind_shared_request;
  bind_shared_request.token() = std::move(attached_token_client);
  bind_shared_request.buffer_collection_request() = std::move(attached_collection_server);
  auto bind_result = allocator->BindSharedCollection(std::move(bind_shared_request));
  ASSERT_TRUE(bind_result.is_ok());
  auto sync_result_3 = attached_collection->Sync();
  ASSERT_TRUE(sync_result_3.is_ok());

  zx::eventpair attached_lifetime_client, attached_lifetime_server;
  zx::eventpair::create(/*options=*/0, &attached_lifetime_client, &attached_lifetime_server);
  // With a buffers_remaining of 0, normally this would require 0 buffers remaining in the
  // LogicalBuffercollection to close attached_lifetime_server, but because we're about to force
  // logical allocation failure, it'll close as soon as we hit logical allocation failure for the
  // attached token.  The logical allocation failure of the attached token doesn't impact collection
  // in any way.
  fuchsia_sysmem2::BufferCollectionAttachLifetimeTrackingRequest attach_request_2;
  attach_request_2.server_end() = std::move(attached_lifetime_server);
  attach_request_2.buffers_remaining() = 0;
  ASSERT_TRUE(attached_collection->AttachLifetimeTracking(std::move(attach_request_2)).is_ok());
  fuchsia_sysmem2::BufferCollectionConstraints attached_constraints;
  attached_constraints.usage().emplace();
  attached_constraints.usage()->cpu() =
      fuchsia_sysmem::wire::kCpuUsageReadOften | fuchsia_sysmem::wire::kCpuUsageWriteOften;
  // We won't be able to logically allocate, because original allocation didn't make room for this
  // buffer.
  attached_constraints.min_buffer_count_for_camping() = 1;
  attached_constraints.buffer_memory_constraints().emplace();
  auto& buffer_memory_2 = attached_constraints.buffer_memory_constraints().value();
  buffer_memory_2.min_size_bytes() = 64 * 1024;
  buffer_memory_2.max_size_bytes() = 128 * 1024;
  buffer_memory_2.physically_contiguous_required() = false;
  buffer_memory_2.secure_required() = false;
  buffer_memory_2.ram_domain_supported() = false;
  buffer_memory_2.cpu_domain_supported() = true;
  buffer_memory_2.inaccessible_domain_supported() = false;
  ZX_DEBUG_ASSERT(!buffer_memory_2.heap_permitted().has_value());
  ZX_DEBUG_ASSERT(!attached_constraints.image_format_constraints().has_value());
  fuchsia_sysmem2::BufferCollectionSetConstraintsRequest set_constraints_request;
  set_constraints_request.constraints() = std::move(attached_constraints);
  auto attached_set_constraints_result =
      attached_collection->SetConstraints(std::move(set_constraints_request));
  ASSERT_TRUE(attached_set_constraints_result.is_ok());
  zx_signals_t attached_pending_signals;
  zx_status_t status = attached_lifetime_client.wait_one(
      ZX_EVENTPAIR_PEER_CLOSED, zx::time::infinite(), &attached_pending_signals);
  ASSERT_OK(status);
  ASSERT_TRUE(attached_pending_signals & ZX_EVENTPAIR_PEER_CLOSED);

  collection.value() = {};

  // ZX_EVENTPAIR_PEER_CLOSED should be seen for eventpair(s) >= kNumBuffers.
  for (uint32_t i = 0; i < kNumEventpairs; ++i) {
    zx_signals_t pending_signals;
    status = client[i].wait_one(ZX_EVENTPAIR_PEER_CLOSED,
                                i >= kNumBuffers ? zx::time::infinite() : zx::time::infinite_past(),
                                &pending_signals);
    ASSERT_TRUE(status == (i >= kNumBuffers) ? ZX_OK : ZX_ERR_TIMED_OUT);
    ASSERT_TRUE(!!(pending_signals & ZX_EVENTPAIR_PEER_CLOSED) == (i >= kNumBuffers));
  }

  for (uint32_t j = 0; j < kNumBuffers; ++j) {
    info.buffers()->at(j).vmo().reset();
    for (uint32_t i = 0; i < kNumBuffers; ++i) {
      zx_signals_t pending_signals;
      status = client[i].wait_one(
          ZX_EVENTPAIR_PEER_CLOSED,
          i >= kNumBuffers - (j + 1) ? zx::time::infinite() : zx::time::infinite_past(),
          &pending_signals);
      ASSERT_TRUE(status == (i >= kNumBuffers - (j + 1)) ? ZX_OK : ZX_ERR_TIMED_OUT);
      ASSERT_TRUE(!!(pending_signals & ZX_EVENTPAIR_PEER_CLOSED) == (i >= kNumBuffers - (j + 1)),
                  "");
    }
  }
}

TEST(Sysmem, TokenOneParticipantWithImageConstraintsV2) {
  auto collection = make_single_participant_collection_v2();
  ASSERT_TRUE(collection.is_ok());

  fuchsia_sysmem2::BufferCollectionConstraints constraints;
  constraints.usage().emplace();
  constraints.usage()->cpu() =
      fuchsia_sysmem::wire::kCpuUsageReadOften | fuchsia_sysmem::wire::kCpuUsageWriteOften;
  constraints.min_buffer_count_for_camping() = 3;
  constraints.buffer_memory_constraints().emplace();
  auto& buffer_memory = constraints.buffer_memory_constraints().value();
  // This min_size_bytes is intentionally too small to hold the min_coded_width and
  // min_coded_height in NV12
  // format.
  buffer_memory.min_size_bytes() = 64 * 1024;
  buffer_memory.max_size_bytes() = 128 * 1024;
  buffer_memory.physically_contiguous_required() = false;
  buffer_memory.secure_required() = false;
  buffer_memory.ram_domain_supported() = false;
  buffer_memory.cpu_domain_supported() = true;
  buffer_memory.inaccessible_domain_supported() = false;
  ZX_DEBUG_ASSERT(!buffer_memory.heap_permitted().has_value());
  constraints.image_format_constraints().emplace(1);
  auto& image_constraints = constraints.image_format_constraints()->at(0);
  image_constraints.pixel_format() = fuchsia_images2::PixelFormat::kNv12;
  image_constraints.color_spaces().emplace(1);
  image_constraints.color_spaces()->at(0) = fuchsia_images2::ColorSpace::kRec709;
  // The min dimensions intentionally imply a min size that's larger than
  // buffer_memory_constraints.min_size_bytes.
  image_constraints.min_size() = {256, 256};
  image_constraints.max_size() = {std::numeric_limits<uint32_t>::max(),
                                  std::numeric_limits<uint32_t>::max()};
  image_constraints.min_bytes_per_row() = 256;
  image_constraints.max_bytes_per_row() = std::numeric_limits<uint32_t>::max();
  image_constraints.max_surface_width_times_surface_height() = std::numeric_limits<uint32_t>::max();
  image_constraints.size_alignment() = {2, 2};
  image_constraints.bytes_per_row_divisor() = 2;
  image_constraints.start_offset_divisor() = 2;
  image_constraints.display_rect_alignment() = {1, 1};

  // TODO(fxb/115937): Make this work for sysmem2.
#if 0
#if SYSMEM_FUZZ_CORPUS
  FILE* ofp = fopen("/cache/sysmem_fuzz_corpus_buffer_collecton_constraints.dat", "wb");
  if (ofp) {
    fwrite(&constraints, sizeof(fuchsia_sysmem::wire::BufferCollectionConstraints), 1, ofp);
    fclose(ofp);
  } else {
    printf("Failed to write sysmem BufferCollectionConstraints corpus file.\n");
    fflush(stderr);
  }
#endif  // SYSMEM_FUZZ_CORPUS
#endif

  fuchsia_sysmem2::BufferCollectionSetConstraintsRequest request;
  request.constraints() = std::move(constraints);
  ASSERT_TRUE(collection->SetConstraints(std::move(request)).is_ok());

  auto allocation_result = collection->WaitForAllBuffersAllocated();
  // This is the first round-trip to/from sysmem.  A failure here can be due
  // to any step above failing async.
  ASSERT_TRUE(allocation_result.is_ok());

  auto& buffer_collection_info = allocation_result->buffer_collection_info().value();

  ASSERT_TRUE(buffer_collection_info.buffers().has_value());
  ASSERT_EQ(buffer_collection_info.buffers()->size(), 3);
  // The size should be sufficient for the whole NV12 frame, not just min_size_bytes.
  ASSERT_TRUE(buffer_collection_info.settings().has_value());
  ASSERT_TRUE(buffer_collection_info.settings()->buffer_settings().has_value());
  ASSERT_TRUE(buffer_collection_info.settings()->buffer_settings()->size_bytes().has_value());
  ASSERT_EQ(buffer_collection_info.settings()->buffer_settings()->size_bytes().value(),
            64 * 1024 * 3 / 2);
  ASSERT_TRUE(
      buffer_collection_info.settings()->buffer_settings()->is_physically_contiguous().has_value());
  ASSERT_EQ(
      buffer_collection_info.settings()->buffer_settings()->is_physically_contiguous().value(),
      false);
  ASSERT_TRUE(buffer_collection_info.settings()->buffer_settings()->is_secure().has_value());
  ASSERT_EQ(buffer_collection_info.settings()->buffer_settings()->is_secure().value(), false);
  ASSERT_TRUE(buffer_collection_info.settings()->buffer_settings()->coherency_domain().has_value());
  ASSERT_EQ(buffer_collection_info.settings()->buffer_settings()->coherency_domain().value(),
            fuchsia_sysmem2::CoherencyDomain::kCpu);
  // We specified image_format_constraints so the result must also have
  // image_format_constraints.
  ASSERT_TRUE(buffer_collection_info.settings()->image_format_constraints().has_value());

  for (uint32_t i = 0; i < buffer_collection_info.buffers()->size(); ++i) {
    ASSERT_NE(buffer_collection_info.buffers()->at(i).vmo()->get(), ZX_HANDLE_INVALID);
    uint64_t size_bytes = 0;
    auto status =
        zx_vmo_get_size(buffer_collection_info.buffers()->at(i).vmo()->get(), &size_bytes);
    ASSERT_EQ(status, ZX_OK);
    // The portion of the VMO the client can use is large enough to hold the min image size,
    // despite the min buffer size being smaller.
    ASSERT_GE(buffer_collection_info.settings()->buffer_settings()->size_bytes().value(),
              64 * 1024 * 3 / 2);
    // The vmo has room for the nominal size of the portion of the VMO the client can use.
    ASSERT_LE(buffer_collection_info.buffers()->at(i).vmo_usable_start().value() +
                  buffer_collection_info.settings()->buffer_settings()->size_bytes().value(),
              size_bytes);
  }
}

TEST(Sysmem, MinBufferCountV2) {
  auto collection = make_single_participant_collection_v2();

  fuchsia_sysmem2::BufferCollectionConstraints constraints;
  constraints.usage().emplace();
  constraints.usage()->cpu() =
      fuchsia_sysmem::wire::kCpuUsageReadOften | fuchsia_sysmem::wire::kCpuUsageWriteOften;
  constraints.min_buffer_count_for_camping() = 3;
  constraints.min_buffer_count() = 5;
  constraints.buffer_memory_constraints().emplace();
  auto& buffer_memory = constraints.buffer_memory_constraints().value();
  buffer_memory.min_size_bytes() = 64 * 1024;
  buffer_memory.max_size_bytes() = 128 * 1024;
  buffer_memory.physically_contiguous_required() = false;
  buffer_memory.secure_required() = false;
  buffer_memory.ram_domain_supported() = false;
  buffer_memory.cpu_domain_supported() = true;
  buffer_memory.inaccessible_domain_supported() = false;
  ZX_DEBUG_ASSERT(!buffer_memory.heap_permitted().has_value());
  ZX_DEBUG_ASSERT(!constraints.image_format_constraints().has_value());

  fuchsia_sysmem2::BufferCollectionSetConstraintsRequest request;
  request.constraints() = std::move(constraints);
  ASSERT_TRUE(collection->SetConstraints(std::move(request)).is_ok());

  auto allocation_result = collection->WaitForAllBuffersAllocated();
  // This is the first round-trip to/from sysmem.  A failure here can be due
  // to any step above failing async.
  ASSERT_TRUE(allocation_result.is_ok());
  ASSERT_TRUE(allocation_result->buffer_collection_info().has_value());
  ASSERT_TRUE(allocation_result->buffer_collection_info()->buffers().has_value());
  ASSERT_EQ(allocation_result->buffer_collection_info()->buffers()->size(), 5);
}

TEST(Sysmem, BufferNameV2) {
  auto collection = make_single_participant_collection_v2();

  const char kSysmemName[] = "abcdefghijkl\0mnopqrstuvwxyz";
  const char kLowPrioName[] = "low_pri";

  // Override default set in make_single_participant_collection_v2)
  fuchsia_sysmem2::NodeSetNameRequest set_name_request;
  set_name_request.name() = kSysmemName;
  set_name_request.priority() = 2000000;
  ASSERT_TRUE(collection->SetName(std::move(set_name_request)).is_ok());

  fuchsia_sysmem2::NodeSetNameRequest set_name_request2;
  set_name_request2.name() = kLowPrioName;
  set_name_request2.priority() = 0;
  ASSERT_TRUE(collection->SetName(std::move(set_name_request2)).is_ok());

  fuchsia_sysmem2::BufferCollectionConstraints constraints;
  constraints.usage().emplace();
  constraints.usage()->cpu() =
      fuchsia_sysmem::wire::kCpuUsageReadOften | fuchsia_sysmem::wire::kCpuUsageWriteOften;
  constraints.min_buffer_count() = 1;
  constraints.buffer_memory_constraints().emplace();
  auto& buffer_memory = constraints.buffer_memory_constraints().value();
  buffer_memory.min_size_bytes() = 4 * 1024;
  buffer_memory.physically_contiguous_required() = false;
  buffer_memory.secure_required() = false;
  buffer_memory.ram_domain_supported() = false;
  buffer_memory.cpu_domain_supported() = true;
  buffer_memory.inaccessible_domain_supported() = false;
  ZX_DEBUG_ASSERT(!buffer_memory.heap_permitted().has_value());
  ZX_DEBUG_ASSERT(!constraints.image_format_constraints().has_value());

  fuchsia_sysmem2::BufferCollectionSetConstraintsRequest set_constraints_request;
  set_constraints_request.constraints() = std::move(constraints);
  ASSERT_TRUE(collection->SetConstraints(std::move(set_constraints_request)).is_ok());

  auto allocation_result = collection->WaitForAllBuffersAllocated();
  // This is the first round-trip to/from sysmem.  A failure here can be due
  // to any step above failing async.
  ASSERT_TRUE(allocation_result.is_ok());

  ASSERT_TRUE(allocation_result->buffer_collection_info().has_value());
  ASSERT_TRUE(allocation_result->buffer_collection_info()->buffers().has_value());
  ASSERT_EQ(allocation_result->buffer_collection_info()->buffers()->size(), 1);
  zx_handle_t vmo = allocation_result->buffer_collection_info()->buffers()->at(0).vmo()->get();
  char vmo_name[ZX_MAX_NAME_LEN];
  ASSERT_EQ(ZX_OK, zx_object_get_property(vmo, ZX_PROP_NAME, vmo_name, sizeof(vmo_name)));

  // Should be equal up to the first null, plus an index
  EXPECT_EQ(std::string("abcdefghijkl:0"), std::string(vmo_name));
  EXPECT_EQ(0u, vmo_name[ZX_MAX_NAME_LEN - 1]);
}

TEST(Sysmem, NoTokenV2) {
  auto allocator = connect_to_sysmem_driver_v2();
  zx::result collection_endpoints = fidl::CreateEndpoints<fuchsia_sysmem2::BufferCollection>();
  ASSERT_TRUE(collection_endpoints.is_ok());
  auto [collection_client_end, collection_server_end] = std::move(*collection_endpoints);
  fidl::SyncClient collection{std::move(collection_client_end)};

  fuchsia_sysmem2::AllocatorAllocateNonSharedCollectionRequest allocate_non_shared_request;
  allocate_non_shared_request.collection_request() = std::move(collection_server_end);
  ASSERT_TRUE(
      allocator->AllocateNonSharedCollection(std::move(allocate_non_shared_request)).is_ok());

  SetDefaultCollectionNameV2(collection);

  fuchsia_sysmem2::BufferCollectionConstraints constraints;
  constraints.usage().emplace();
  constraints.usage()->cpu() =
      fuchsia_sysmem::wire::kCpuUsageReadOften | fuchsia_sysmem::wire::kCpuUsageWriteOften;
  // Ask for display usage to encourage using the ram coherency domain.
  constraints.usage()->display() = fuchsia_sysmem::wire::kDisplayUsageLayer;
  constraints.min_buffer_count_for_camping() = 3;
  constraints.buffer_memory_constraints().emplace();
  auto& buffer_memory = constraints.buffer_memory_constraints().value();
  buffer_memory.min_size_bytes() = 64 * 1024;
  buffer_memory.max_size_bytes() = 128 * 1024;
  buffer_memory.physically_contiguous_required() = false;
  buffer_memory.secure_required() = false;
  buffer_memory.ram_domain_supported() = true;
  buffer_memory.cpu_domain_supported() = true;
  buffer_memory.inaccessible_domain_supported() = false;
  ZX_DEBUG_ASSERT(!buffer_memory.heap_permitted().has_value());
  ZX_DEBUG_ASSERT(!constraints.image_format_constraints().has_value());

  fuchsia_sysmem2::BufferCollectionSetConstraintsRequest set_constraints_request;
  set_constraints_request.constraints() = std::move(constraints);
  ASSERT_TRUE(collection->SetConstraints(std::move(set_constraints_request)).is_ok());

  auto allocation_result = collection->WaitForAllBuffersAllocated();
  // This is the first round-trip to/from sysmem.  A failure here can be due
  // to any step above failing async.
  ASSERT_TRUE(allocation_result.is_ok());

  auto& buffer_collection_info = allocation_result->buffer_collection_info().value();
  ASSERT_TRUE(buffer_collection_info.buffers().has_value());
  ASSERT_EQ(buffer_collection_info.buffers()->size(), 3);
  ASSERT_TRUE(buffer_collection_info.settings().has_value());
  ASSERT_TRUE(buffer_collection_info.settings()->buffer_settings().has_value());
  ASSERT_TRUE(buffer_collection_info.settings()->buffer_settings()->size_bytes().has_value());
  ASSERT_EQ(buffer_collection_info.settings()->buffer_settings()->size_bytes().value(), 64 * 1024);
  ASSERT_TRUE(
      buffer_collection_info.settings()->buffer_settings()->is_physically_contiguous().has_value());
  ASSERT_EQ(buffer_collection_info.settings()->buffer_settings()->is_physically_contiguous(),
            false);
  ASSERT_TRUE(buffer_collection_info.settings()->buffer_settings()->is_secure().has_value());
  ASSERT_EQ(buffer_collection_info.settings()->buffer_settings()->is_secure(), false);
  ASSERT_TRUE(buffer_collection_info.settings()->buffer_settings()->coherency_domain().has_value());
  ASSERT_EQ(buffer_collection_info.settings()->buffer_settings()->coherency_domain(),
            fuchsia_sysmem2::CoherencyDomain::kRam);
  ASSERT_FALSE(buffer_collection_info.settings()->image_format_constraints().has_value());

  for (uint32_t i = 0; i < buffer_collection_info.buffers()->size(); ++i) {
    ASSERT_NE(buffer_collection_info.buffers()->at(i).vmo()->get(), ZX_HANDLE_INVALID);
    uint64_t size_bytes = 0;
    auto status =
        zx_vmo_get_size(buffer_collection_info.buffers()->at(i).vmo()->get(), &size_bytes);
    ASSERT_EQ(status, ZX_OK);
    ASSERT_EQ(size_bytes, 64 * 1024);
  }
}

TEST(Sysmem, NoSyncV2) {
  auto allocator_1 = connect_to_sysmem_driver_v2();
  ASSERT_OK(allocator_1);

  auto token_endpoints_1 = fidl::CreateEndpoints<v2::BufferCollectionToken>();
  ASSERT_OK(token_endpoints_1);
  auto [token_client_1, token_server_1] = std::move(*token_endpoints_1);
  fidl::SyncClient token_1{std::move(token_client_1)};

  v2::AllocatorAllocateSharedCollectionRequest request;
  request.token_request() = std::move(token_server_1);
  ASSERT_TRUE(allocator_1->AllocateSharedCollection(std::move(request)).is_ok());

  const char* kAllocatorName = "TestAllocator";
  v2::AllocatorSetDebugClientInfoRequest allocator_set_debug_info_request;
  allocator_set_debug_info_request.name() = kAllocatorName;
  allocator_set_debug_info_request.id() = 1u;
  ASSERT_TRUE(allocator_1->SetDebugClientInfo(std::move(allocator_set_debug_info_request)).is_ok());

  const char* kClientName = "TestClient";
  v2::NodeSetDebugClientInfoRequest token_set_debug_info_request2;
  token_set_debug_info_request2.name() = kClientName;
  token_set_debug_info_request2.id() = 2u;
  ASSERT_TRUE(token_1->SetDebugClientInfo(std::move(token_set_debug_info_request2)).is_ok());

  // Make another token so we can bind it and set a name on the collection.
  fidl::SyncClient<v2::BufferCollection> collection_3;

  {
    auto token_endpoints_3 = fidl::CreateEndpoints<v2::BufferCollectionToken>();
    ASSERT_OK(token_endpoints_3);
    auto [token_client_3, token_server_3] = std::move(*token_endpoints_3);

    auto collection_endpoints_3 = fidl::CreateEndpoints<v2::BufferCollection>();
    ASSERT_OK(collection_endpoints_3);
    auto [collection_client_3, collection_server_3] = std::move(*collection_endpoints_3);

    v2::BufferCollectionTokenDuplicateRequest duplicate_request;
    duplicate_request.rights_attenuation_mask() = ZX_RIGHT_SAME_RIGHTS;
    duplicate_request.token_request() = std::move(token_server_3);
    ASSERT_TRUE(token_1->Duplicate(std::move(duplicate_request)).is_ok());
    ASSERT_TRUE(token_1->Sync().is_ok());

    v2::AllocatorBindSharedCollectionRequest allocator_bind_request;
    allocator_bind_request.token() = std::move(token_client_3);
    allocator_bind_request.buffer_collection_request() = std::move(collection_server_3);
    ASSERT_TRUE(allocator_1->BindSharedCollection(std::move(allocator_bind_request)).is_ok());

    collection_3 = fidl::SyncClient(std::move(collection_client_3));

    const char* kCollectionName = "TestCollection";

    v2::NodeSetNameRequest set_name_request;
    set_name_request.priority() = 1u;
    set_name_request.name() = kCollectionName;
    ASSERT_TRUE(collection_3->SetName(std::move(set_name_request)).is_ok());
  }

  auto token_endpoints_2 = fidl::CreateEndpoints<v2::BufferCollectionToken>();
  ASSERT_OK(token_endpoints_2);
  auto [token_client_2, token_server_2] = std::move(*token_endpoints_2);
  fidl::SyncClient token_2{std::move(token_client_2)};

  auto collection_endpoints_1 = fidl::CreateEndpoints<v2::BufferCollection>();
  ASSERT_OK(collection_endpoints_1);
  auto [collection_client_1, collection_server_1] = std::move(*collection_endpoints_1);
  fidl::SyncClient collection_1{std::move(collection_client_1)};

  const char* kClient2Name = "TestClient2";
  v2::NodeSetDebugClientInfoRequest set_debug_request;
  set_debug_request.name() = kClient2Name;
  set_debug_request.id() = 3u;
  ASSERT_TRUE(token_2->SetDebugClientInfo(std::move(set_debug_request)).is_ok());

  // Close to prevent Sync on token_client_1 from failing later due to LogicalBufferCollection
  // failure caused by the token handle closing.
  ASSERT_TRUE(token_2->Close().is_ok());

  v2::AllocatorBindSharedCollectionRequest bind_shared_request;
  bind_shared_request.token() = token_2.TakeClientEnd();
  bind_shared_request.buffer_collection_request() = std::move(collection_server_1);
  ASSERT_TRUE(allocator_1->BindSharedCollection(std::move(bind_shared_request)).is_ok());

  // Duplicate has not been sent (or received) so this should fail.
  auto sync_result = collection_1->Sync();
  EXPECT_FALSE(sync_result.is_ok());
  EXPECT_STATUS(sync_result.error_value().status(), ZX_ERR_PEER_CLOSED);

  // The duplicate/sync should print out an error message but succeed.
  v2::BufferCollectionTokenDuplicateRequest duplicate_request;
  duplicate_request.rights_attenuation_mask() = ZX_RIGHT_SAME_RIGHTS;
  duplicate_request.token_request() = std::move(token_server_2);
  ASSERT_TRUE(token_1->Duplicate(std::move(duplicate_request)).is_ok());
  ASSERT_TRUE(token_1->Sync().is_ok());
}

TEST(Sysmem, MultipleParticipantsV2) {
  auto allocator = connect_to_sysmem_driver_v2();

  auto token_endpoints_1 = fidl::CreateEndpoints<v2::BufferCollectionToken>();
  ASSERT_OK(token_endpoints_1);
  auto [token_client_1, token_server_1] = std::move(*token_endpoints_1);
  fidl::SyncClient token_1{std::move(token_client_1)};

  // Client 1 creates a token and new LogicalBufferCollection using
  // AllocateSharedCollection().
  v2::AllocatorAllocateSharedCollectionRequest allocate_shared_request;
  allocate_shared_request.token_request() = std::move(token_server_1);
  ASSERT_TRUE(allocator->AllocateSharedCollection(std::move(allocate_shared_request)).is_ok());

  auto token_endpoints_2 = fidl::CreateEndpoints<v2::BufferCollectionToken>();
  ASSERT_OK(token_endpoints_2);
  auto [token_client_2, token_server_2] = std::move(*token_endpoints_2);

  // Client 1 duplicates its token and gives the duplicate to client 2 (this
  // test is single proc, so both clients are coming from this client
  // process - normally the two clients would be in separate processes with
  // token_client_2 transferred to another participant).
  v2::BufferCollectionTokenDuplicateRequest duplicate_request;
  duplicate_request.rights_attenuation_mask() = ZX_RIGHT_SAME_RIGHTS;
  duplicate_request.token_request() = std::move(token_server_2);
  ASSERT_TRUE(token_1->Duplicate(std::move(duplicate_request)).is_ok());

  auto token_endpoints_3 = fidl::CreateEndpoints<v2::BufferCollectionToken>();
  ASSERT_OK(token_endpoints_3);
  auto [token_client_3, token_server_3] = std::move(*token_endpoints_3);

  // Client 3 is used to test a participant that doesn't set any constraints
  // and only wants a notification that the allocation is done.
  v2::BufferCollectionTokenDuplicateRequest duplicate_request2;
  duplicate_request2.rights_attenuation_mask() = ZX_RIGHT_SAME_RIGHTS;
  duplicate_request2.token_request() = std::move(token_server_3);
  ASSERT_TRUE(token_1->Duplicate(std::move(duplicate_request2)).is_ok());

  auto collection_endpoints_1 = fidl::CreateEndpoints<v2::BufferCollection>();
  ASSERT_OK(collection_endpoints_1);
  auto [collection_client_1, collection_server_1] = std::move(*collection_endpoints_1);
  fidl::SyncClient collection_1{std::move(collection_client_1)};

  ASSERT_NE(token_1.client_end().channel().get(), ZX_HANDLE_INVALID);
  v2::AllocatorBindSharedCollectionRequest bind_shared_request;
  bind_shared_request.token() = token_1.TakeClientEnd();
  bind_shared_request.buffer_collection_request() = std::move(collection_server_1);
  ASSERT_TRUE(allocator->BindSharedCollection(std::move(bind_shared_request)).is_ok());

  SetDefaultCollectionNameV2(collection_1);

  v2::BufferCollectionConstraints constraints_1;
  constraints_1.usage().emplace();
  constraints_1.usage()->cpu() =
      fuchsia_sysmem::wire::kCpuUsageReadOften | fuchsia_sysmem::wire::kCpuUsageWriteOften;
  constraints_1.min_buffer_count_for_camping() = 3;
  constraints_1.buffer_memory_constraints().emplace();
  auto& buffer_memory = constraints_1.buffer_memory_constraints().value();
  // This min_size_bytes is intentionally too small to hold the min_coded_width and
  // min_coded_height in NV12
  // format.
  buffer_memory.min_size_bytes() = 64 * 1024;
  // Allow a max that's just large enough to accommodate the size implied
  // by the min frame size and PixelFormat.
  buffer_memory.max_size_bytes() = (512 * 512) * 3 / 2;
  buffer_memory.physically_contiguous_required() = false;
  buffer_memory.secure_required() = false;
  buffer_memory.ram_domain_supported() = false;
  buffer_memory.cpu_domain_supported() = true;
  buffer_memory.inaccessible_domain_supported() = false;
  ZX_DEBUG_ASSERT(!buffer_memory.heap_permitted().has_value());
  constraints_1.image_format_constraints().emplace(1);
  auto& image_constraints_1 = constraints_1.image_format_constraints()->at(0);
  image_constraints_1.pixel_format() = fuchsia_images2::PixelFormat::kNv12;
  image_constraints_1.color_spaces().emplace(1);
  image_constraints_1.color_spaces()->at(0) = fuchsia_images2::ColorSpace::kRec709;
  // The min dimensions intentionally imply a min size that's larger than
  // buffer_memory_constraints.min_size_bytes.
  image_constraints_1.min_size() = {256, 256};
  image_constraints_1.max_size() = {std::numeric_limits<uint32_t>::max(),
                                    std::numeric_limits<uint32_t>::max()};
  image_constraints_1.min_bytes_per_row() = 256;
  image_constraints_1.max_bytes_per_row() = std::numeric_limits<uint32_t>::max();
  image_constraints_1.max_surface_width_times_surface_height() =
      std::numeric_limits<uint32_t>::max();
  image_constraints_1.size_alignment() = {2, 2};
  image_constraints_1.bytes_per_row_divisor() = 2;
  image_constraints_1.start_offset_divisor() = 2;
  image_constraints_1.display_rect_alignment() = {1, 1};

  // Start with constraints_2 a copy of constraints_1.  There are no handles
  // in the constraints struct so a struct copy instead of clone is fine here.
  v2::BufferCollectionConstraints constraints_2 = constraints_1;
  // Modify constraints_2 to require double the width and height.
  constraints_2.image_format_constraints()->at(0).min_size() = {512, 512};

  // TODO(fxb/115937): Make this work for sysmem2.
#if 0
#if SYSMEM_FUZZ_CORPUS
  FILE* ofp = fopen("/cache/sysmem_fuzz_corpus_multi_buffer_collecton_constraints.dat", "wb");
  if (ofp) {
    fwrite(&constraints_1, sizeof(fuchsia_sysmem::wire::BufferCollectionConstraints), 1, ofp);
    fwrite(&constraints_2, sizeof(fuchsia_sysmem::wire::BufferCollectionConstraints), 1, ofp);
    fclose(ofp);
  } else {
    printf("Failed to write sysmem multi BufferCollectionConstraints corpus file.\n");
    fflush(stderr);
  }
#endif  // SYSMEM_FUZZ_CORPUS
#endif

  v2::BufferCollectionSetConstraintsRequest set_constraints_request;
  set_constraints_request.constraints() = std::move(constraints_1);
  ASSERT_TRUE(collection_1->SetConstraints(std::move(set_constraints_request)).is_ok());

  // Client 2 connects to sysmem separately.
  auto allocator_2 = connect_to_sysmem_driver_v2();
  ASSERT_OK(allocator_2);

  auto collection_endpoints_2 = fidl::CreateEndpoints<v2::BufferCollection>();
  ASSERT_OK(collection_endpoints_2);
  auto [collection_client_2, collection_server_2] = std::move(*collection_endpoints_2);
  fidl::SyncClient collection_2{std::move(collection_client_2)};

  // Just because we can, perform this sync as late as possible, just before
  // the BindSharedCollection() via allocator2_client_2.  Without this Sync(),
  // the BindSharedCollection() might arrive at the server before the
  // Duplicate() that delivered the server end of token_client_2 to sysmem,
  // which would cause sysmem to not recognize the token.
  ASSERT_TRUE(collection_1->Sync().is_ok());

  // For the moment, cause the server to count some fake churn, enough times to cause the server
  // to re-alloc all the server's held FIDL tables 4 times before we continue.  These are
  // synchronous calls, so the 4 re-allocs are done by the time this loop completes.
  //
  // TODO(fxbug.dev/33670): Switch to creating real churn instead, once we have new messages that
  // can create real churn.
  constexpr uint32_t kChurnCount = 256 * 2;  // 256 * 4;
  for (uint32_t i = 0; i < kChurnCount; ++i) {
    ASSERT_TRUE(collection_1->Sync().is_ok());
  }

  ASSERT_NE(token_client_2.channel().get(), ZX_HANDLE_INVALID);

  v2::AllocatorBindSharedCollectionRequest bind_shared_request2;
  bind_shared_request2.token() = std::move(token_client_2);
  bind_shared_request2.buffer_collection_request() = std::move(collection_server_2);
  ASSERT_TRUE(allocator_2->BindSharedCollection(std::move(bind_shared_request2)).is_ok());

  auto collection_endpoints_3 = fidl::CreateEndpoints<v2::BufferCollection>();
  ASSERT_OK(collection_endpoints_3);
  auto [collection_client_3, collection_server_3] = std::move(*collection_endpoints_3);
  fidl::SyncClient collection_3{std::move(collection_client_3)};

  ASSERT_NE(token_client_3.channel().get(), ZX_HANDLE_INVALID);

  v2::AllocatorBindSharedCollectionRequest bind_shared_request3;
  bind_shared_request3.token() = std::move(token_client_3);
  bind_shared_request3.buffer_collection_request() = std::move(collection_server_3);
  ASSERT_TRUE(allocator_2->BindSharedCollection(std::move(bind_shared_request3)).is_ok());

  v2::BufferCollectionConstraints empty_constraints;
  v2::BufferCollectionSetConstraintsRequest set_constraints_request2;
  set_constraints_request2.constraints() = std::move(empty_constraints);
  ASSERT_TRUE(collection_3->SetConstraints(std::move(set_constraints_request2)).is_ok());

  // Not all constraints have been input, so the buffers haven't been
  // allocated yet.
  auto check_result_1 = collection_1->CheckAllBuffersAllocated();
  ASSERT_FALSE(check_result_1.is_ok());
  ASSERT_TRUE(check_result_1.error_value().is_domain_error());
  EXPECT_EQ(check_result_1.error_value().domain_error(), ZX_ERR_UNAVAILABLE);
  auto check_result_2 = collection_2->CheckAllBuffersAllocated();
  ASSERT_FALSE(check_result_2.is_ok());
  ASSERT_TRUE(check_result_2.error_value().is_domain_error());
  EXPECT_EQ(check_result_2.error_value().domain_error(), ZX_ERR_UNAVAILABLE);

  v2::BufferCollectionSetConstraintsRequest set_constraints_request3;
  set_constraints_request3.constraints() = std::move(constraints_2);
  ASSERT_TRUE(collection_2->SetConstraints(std::move(set_constraints_request3)).is_ok());

  //
  // Only after both participants (both clients) have SetConstraints() will
  // the allocation be successful.
  //

  auto allocate_result_1 = collection_1->WaitForAllBuffersAllocated();
  // This is the first round-trip to/from sysmem.  A failure here can be due
  // to any step above failing async.
  ASSERT_TRUE(allocate_result_1.is_ok());

  auto check_result_allocated_1 = collection_1->CheckAllBuffersAllocated();
  ASSERT_TRUE(check_result_allocated_1.is_ok());
  auto check_result_allocated_2 = collection_2->CheckAllBuffersAllocated();
  ASSERT_TRUE(check_result_allocated_2.is_ok());

  auto allocate_result_2 = collection_2->WaitForAllBuffersAllocated();
  ASSERT_TRUE(allocate_result_2.is_ok());

  auto allocate_result_3 = collection_3->WaitForAllBuffersAllocated();
  ASSERT_TRUE(allocate_result_3.is_ok());

  //
  // buffer_collection_info_1 and buffer_collection_info_2 should be exactly
  // equal except their non-zero handle values, which should be different.  We
  // verify the handle values then check that the structs are exactly the same
  // with handle values zeroed out.
  //
  auto& buffer_collection_info_1 = allocate_result_1->buffer_collection_info().value();
  auto& buffer_collection_info_2 = allocate_result_2->buffer_collection_info().value();
  auto& buffer_collection_info_3 = allocate_result_3->buffer_collection_info().value();

  for (uint32_t i = 0; i < buffer_collection_info_1.buffers()->size(); ++i) {
    ASSERT_EQ(buffer_collection_info_1.buffers()->at(i).vmo()->get() != ZX_HANDLE_INVALID,
              buffer_collection_info_2.buffers()->at(i).vmo()->get() != ZX_HANDLE_INVALID);
    if (buffer_collection_info_1.buffers()->at(i).vmo()->get() != ZX_HANDLE_INVALID) {
      // The handle values must be different.
      ASSERT_NE(buffer_collection_info_1.buffers()->at(i).vmo()->get(),
                buffer_collection_info_2.buffers()->at(i).vmo()->get());
      // For now, the koid(s) are expected to be equal.  This is not a
      // fundamental check, in that sysmem could legitimately change in
      // future to vend separate child VMOs (of the same portion of a
      // non-copy-on-write parent VMO) to the two participants and that
      // would still be potentially valid overall.
      zx_koid_t koid_1 = get_koid(buffer_collection_info_1.buffers()->at(i).vmo()->get());
      zx_koid_t koid_2 = get_koid(buffer_collection_info_2.buffers()->at(i).vmo()->get());
      ASSERT_EQ(koid_1, koid_2);
    }

    // Buffer collection 3 passed false to SetConstraints(), so we get no VMOs.
    ASSERT_EQ(ZX_HANDLE_INVALID, buffer_collection_info_3.buffers()->at(i).vmo()->get());
  }

  //
  // Verify that buffer_collection_info_1 paid attention to constraints_2, and
  // that buffer_collection_info_2 makes sense.
  //

  // Because each specified min_buffer_count_for_camping 3, and each
  // participant camping count adds together since they camp independently.
  ASSERT_EQ(buffer_collection_info_1.buffers()->size(), 6);
  // The size should be sufficient for the whole NV12 frame, not just
  // min_size_bytes.  In other words, the portion of the VMO the client can
  // use is large enough to hold the min image size, despite the min buffer
  // size being smaller.
  ASSERT_GE(buffer_collection_info_1.settings()->buffer_settings()->size_bytes().value(),
            (512 * 512) * 3 / 2);
  ASSERT_EQ(
      buffer_collection_info_1.settings()->buffer_settings()->is_physically_contiguous().value(),
      false);
  ASSERT_EQ(buffer_collection_info_1.settings()->buffer_settings()->is_secure().value(), false);
  // We specified image_format_constraints so the result must also have
  // image_format_constraints.
  ASSERT_TRUE(buffer_collection_info_1.settings()->image_format_constraints().has_value());

  ASSERT_EQ(buffer_collection_info_1.buffers()->size(), buffer_collection_info_2.buffers()->size());

  for (uint32_t i = 0; i < buffer_collection_info_1.buffers()->size(); ++i) {
    auto& vmo1 = buffer_collection_info_1.buffers()->at(i).vmo().value();
    auto& vmo2 = buffer_collection_info_2.buffers()->at(i).vmo().value();

    ASSERT_NE(vmo1.get(), ZX_HANDLE_INVALID);
    ASSERT_NE(vmo2.get(), ZX_HANDLE_INVALID);

    uint64_t size_bytes_1 = 0;
    ASSERT_OK(vmo1.get_size(&size_bytes_1));

    uint64_t size_bytes_2 = 0;
    ASSERT_OK(zx_vmo_get_size(vmo2.get(), &size_bytes_2));

    // The vmo has room for the nominal size of the portion of the VMO
    // the client can use.  These checks should pass even if sysmem were
    // to vend different child VMOs to the two participants.
    ASSERT_LE(buffer_collection_info_1.buffers()->at(i).vmo_usable_start().value() +
                  buffer_collection_info_1.settings()->buffer_settings()->size_bytes().value(),
              size_bytes_1);
    ASSERT_LE(buffer_collection_info_2.buffers()->at(i).vmo_usable_start().value() +
                  buffer_collection_info_2.settings()->buffer_settings()->size_bytes().value(),
              size_bytes_2);

    // Clear out vmos for compare below.
    buffer_collection_info_1.buffers()->at(i).vmo().reset();
    buffer_collection_info_2.buffers()->at(i).vmo().reset();
  }

  // Check that buffer_collection_info_1 and buffer_collection_info_2 are
  // consistent.
  ASSERT_TRUE(Equal(buffer_collection_info_1, buffer_collection_info_2));

  // Check that buffer_collection_info_1 and buffer_collection_info_3 are
  // consistent, except for the vmos.
  ASSERT_TRUE(Equal(buffer_collection_info_1, buffer_collection_info_3));
}

TEST(Sysmem, ComplicatedFormatModifiersV2) {
  auto allocator = connect_to_sysmem_driver_v2();

  auto token_endpoints_1 = fidl::CreateEndpoints<v2::BufferCollectionToken>();
  ASSERT_TRUE(token_endpoints_1.is_ok());
  auto [token_client_1, token_server_1] = std::move(*token_endpoints_1);
  fidl::SyncClient token_1{std::move(token_client_1)};

  v2::AllocatorAllocateSharedCollectionRequest allocate_shared_request;
  allocate_shared_request.token_request() = std::move(token_server_1);
  ASSERT_TRUE(allocator->AllocateSharedCollection(std::move(allocate_shared_request)).is_ok());

  auto token_endpoints_2 = fidl::CreateEndpoints<v2::BufferCollectionToken>();
  ASSERT_TRUE(token_endpoints_2.is_ok());
  auto [token_client_2, token_server_2] = std::move(*token_endpoints_2);

  v2::BufferCollectionTokenDuplicateRequest duplicate_request;
  duplicate_request.rights_attenuation_mask() = ZX_RIGHT_SAME_RIGHTS;
  duplicate_request.token_request() = std::move(token_server_2);
  ASSERT_TRUE(token_1->Duplicate(std::move(duplicate_request)).is_ok());

  auto collection_endpoints_1 = fidl::CreateEndpoints<v2::BufferCollection>();
  ASSERT_TRUE(collection_endpoints_1.is_ok());
  auto [collection_client_1, collection_server_1] = std::move(*collection_endpoints_1);
  fidl::SyncClient collection_1{std::move(collection_client_1)};

  ASSERT_NE(token_1.client_end().channel().get(), ZX_HANDLE_INVALID);

  v2::AllocatorBindSharedCollectionRequest bind_shared_request;
  bind_shared_request.token() = token_1.TakeClientEnd();
  bind_shared_request.buffer_collection_request() = std::move(collection_server_1);
  ASSERT_TRUE(allocator->BindSharedCollection(std::move(bind_shared_request)).is_ok());

  SetDefaultCollectionNameV2(collection_1);

  v2::BufferCollectionConstraints constraints_1;
  constraints_1.usage().emplace();
  constraints_1.usage()->cpu() =
      fuchsia_sysmem::wire::kCpuUsageReadOften | fuchsia_sysmem::wire::kCpuUsageWriteOften;
  constraints_1.min_buffer_count_for_camping() = 1;

  constexpr uint64_t kFormatModifiers[] = {
      fuchsia_images2::kFormatModifierLinear, fuchsia_images2::kFormatModifierIntelI915XTiled,
      fuchsia_images2::kFormatModifierArmAfbc16X16SplitBlockSparseYuvTeTiledHeader,
      fuchsia_images2::kFormatModifierArmAfbc16X16Te};
  constraints_1.image_format_constraints().emplace(std::size(kFormatModifiers));

  for (uint32_t i = 0; i < std::size(kFormatModifiers); i++) {
    auto& image_constraints_1 = constraints_1.image_format_constraints()->at(i);

    image_constraints_1.pixel_format() =
        i < 2 ? fuchsia_images2::PixelFormat::kR8G8B8A8 : fuchsia_images2::PixelFormat::kBgra32;
    image_constraints_1.pixel_format_modifier() = kFormatModifiers[i];
    image_constraints_1.color_spaces().emplace(1);
    image_constraints_1.color_spaces()->at(0) = fuchsia_images2::ColorSpace::kSrgb;
  }

  // Start with constraints_2 a copy of constraints_1.  There are no handles
  // in the constraints struct so a struct copy instead of clone is fine here.
  v2::BufferCollectionConstraints constraints_2(constraints_1);

  for (uint32_t i = 0; i < constraints_2.image_format_constraints()->size(); ++i) {
    // Modify constraints_2 to require nonzero image dimensions.
    constraints_2.image_format_constraints()->at(i).required_max_size() = {512, 512};
  }

  // TODO(fxb/115937): Make this work for sysmem2.
#if SYSMEM_FUZZ_CORPUS
  FILE* ofp = fopen("/cache/sysmem_fuzz_corpus_multi_buffer_format_modifier_constraints.dat", "wb");
  if (ofp) {
    fwrite(&constraints_1, sizeof(fuchsia_sysmem::wire::BufferCollectionConstraints), 1, ofp);
    fwrite(&constraints_2, sizeof(fuchsia_sysmem::wire::BufferCollectionConstraints), 1, ofp);
    fclose(ofp);
  } else {
    fprintf(stderr,
            "Failed to write sysmem multi BufferCollectionConstraints corpus file at line %d\n",
            __LINE__);
    fflush(stderr);
  }
#endif  // SYSMEM_FUZZ_CORPUS

  v2::BufferCollectionSetConstraintsRequest set_constraints_request;
  set_constraints_request.constraints() = std::move(constraints_1);
  ASSERT_TRUE(collection_1->SetConstraints(std::move(set_constraints_request)).is_ok());

  auto collection_endpoints_2 = fidl::CreateEndpoints<v2::BufferCollection>();
  ASSERT_TRUE(collection_endpoints_2.is_ok());
  auto [collection_client_2, collection_server_2] = std::move(*collection_endpoints_2);
  fidl::SyncClient collection_2{std::move(collection_client_2)};

  ASSERT_TRUE(collection_1->Sync().is_ok());

  ASSERT_NE(token_client_2.channel().get(), ZX_HANDLE_INVALID);

  v2::AllocatorBindSharedCollectionRequest bind_shared_request2;
  bind_shared_request2.token() = std::move(token_client_2);
  bind_shared_request2.buffer_collection_request() = std::move(collection_server_2);
  ASSERT_TRUE(allocator->BindSharedCollection(std::move(bind_shared_request2)).is_ok());

  v2::BufferCollectionSetConstraintsRequest set_constraints_request2;
  set_constraints_request2.constraints() = std::move(constraints_2);
  ASSERT_TRUE(collection_2->SetConstraints(std::move(set_constraints_request2)).is_ok());

  //
  // Only after both participants (both clients) have SetConstraints() will
  // the allocation be successful.
  //
  auto allocate_result_1 = collection_1->WaitForAllBuffersAllocated();
  // This is the first round-trip to/from sysmem.  A failure here can be due
  // to any step above failing async.
  ASSERT_TRUE(allocate_result_1.is_ok());
}

TEST(Sysmem, MultipleParticipantsColorspaceRankingV2) {
  auto allocator = connect_to_sysmem_driver_v2();

  auto token_endpoints_1 = fidl::CreateEndpoints<v2::BufferCollectionToken>();
  ASSERT_OK(token_endpoints_1);
  auto [token_client_1, token_server_1] = std::move(*token_endpoints_1);
  fidl::SyncClient token_1{std::move(token_client_1)};

  v2::AllocatorAllocateSharedCollectionRequest allocate_shared_request;
  allocate_shared_request.token_request() = std::move(token_server_1);
  ASSERT_TRUE(allocator->AllocateSharedCollection(std::move(allocate_shared_request)).is_ok());

  auto token_endpoints_2 = fidl::CreateEndpoints<v2::BufferCollectionToken>();
  ASSERT_TRUE(token_endpoints_2.is_ok());
  auto [token_client_2, token_server_2] = std::move(*token_endpoints_2);

  v2::BufferCollectionTokenDuplicateRequest duplicate_request;
  duplicate_request.rights_attenuation_mask() = ZX_RIGHT_SAME_RIGHTS;
  duplicate_request.token_request() = std::move(token_server_2);
  ASSERT_TRUE(token_1->Duplicate(std::move(duplicate_request)).is_ok());

  auto collection_endpoints_1 = fidl::CreateEndpoints<v2::BufferCollection>();
  ASSERT_TRUE(collection_endpoints_1.is_ok());
  auto [collection_client_1, collection_server_1] = std::move(*collection_endpoints_1);
  fidl::SyncClient collection_1{std::move(collection_client_1)};

  ASSERT_NE(token_1.client_end().channel().get(), ZX_HANDLE_INVALID);

  v2::AllocatorBindSharedCollectionRequest bind_shared_request;
  bind_shared_request.token() = token_1.TakeClientEnd();
  bind_shared_request.buffer_collection_request() = std::move(collection_server_1);
  ASSERT_TRUE(allocator->BindSharedCollection(std::move(bind_shared_request)).is_ok());

  SetDefaultCollectionNameV2(collection_1);

  v2::BufferCollectionConstraints constraints_1;
  constraints_1.usage().emplace();
  constraints_1.usage()->cpu() =
      fuchsia_sysmem::wire::kCpuUsageReadOften | fuchsia_sysmem::wire::kCpuUsageWriteOften;
  constraints_1.min_buffer_count_for_camping() = 1;
  constraints_1.buffer_memory_constraints().emplace();
  auto& buffer_memory = constraints_1.buffer_memory_constraints().value();
  buffer_memory.min_size_bytes() = 64 * 1024;
  buffer_memory.max_size_bytes() = 128 * 1024;
  buffer_memory.cpu_domain_supported() = true;
  constraints_1.image_format_constraints().emplace(1);
  auto& image_constraints_1 = constraints_1.image_format_constraints()->at(0);
  image_constraints_1.pixel_format() = fuchsia_images2::PixelFormat::kNv12;
  image_constraints_1.color_spaces().emplace(3);
  image_constraints_1.color_spaces()->at(0) = fuchsia_images2::ColorSpace::kRec601Pal;
  image_constraints_1.color_spaces()->at(1) = fuchsia_images2::ColorSpace::kRec601PalFullRange;
  image_constraints_1.color_spaces()->at(2) = fuchsia_images2::ColorSpace::kRec709;

  // Start with constraints_2 a copy of constraints_1.  There are no handles
  // in the constraints struct so a struct copy instead of clone is fine here.
  v2::BufferCollectionConstraints constraints_2(constraints_1);
  v2::ImageFormatConstraints& image_constraints_2 = constraints_2.image_format_constraints()->at(0);
  image_constraints_2.color_spaces().emplace(2);
  image_constraints_2.color_spaces()->at(0) = fuchsia_images2::ColorSpace::kRec601PalFullRange;
  image_constraints_2.color_spaces()->at(1) = fuchsia_images2::ColorSpace::kRec709;

  v2::BufferCollectionSetConstraintsRequest set_constraints_request;
  set_constraints_request.constraints() = std::move(constraints_1);
  ASSERT_TRUE(collection_1->SetConstraints(std::move(set_constraints_request)).is_ok());

  auto collection_endpoints_2 = fidl::CreateEndpoints<v2::BufferCollection>();
  ASSERT_TRUE(collection_endpoints_2.is_ok());
  auto [collection_client_2, collection_server_2] = std::move(*collection_endpoints_2);
  fidl::SyncClient collection_2{std::move(collection_client_2)};

  ASSERT_TRUE(collection_1->Sync().is_ok());

  ASSERT_NE(token_client_2.channel().get(), ZX_HANDLE_INVALID);

  v2::AllocatorBindSharedCollectionRequest bind_shared_request2;
  bind_shared_request2.token() = std::move(token_client_2);
  bind_shared_request2.buffer_collection_request() = std::move(collection_server_2);
  ASSERT_TRUE(allocator->BindSharedCollection(std::move(bind_shared_request2)).is_ok());

  v2::BufferCollectionSetConstraintsRequest set_constraints_request2;
  set_constraints_request2.constraints() = std::move(constraints_2);
  ASSERT_TRUE(collection_2->SetConstraints(std::move(set_constraints_request2)).is_ok());

  // Both collections should yield the same results
  auto check_allocation_results = [](const fidl::Result<
                                      fuchsia_sysmem2::BufferCollection::WaitForAllBuffersAllocated>
                                         allocation_result) {
    ASSERT_TRUE(allocation_result.is_ok());

    ASSERT_TRUE(allocation_result->buffer_collection_info().has_value());
    auto& buffer_collection_info = allocation_result->buffer_collection_info().value();
    ASSERT_TRUE(buffer_collection_info.buffers().has_value());
    ASSERT_EQ(buffer_collection_info.buffers()->size(), 2);
    ASSERT_TRUE(buffer_collection_info.settings().has_value());
    ASSERT_TRUE(buffer_collection_info.settings()->image_format_constraints().has_value());
    ASSERT_TRUE(
        buffer_collection_info.settings()->image_format_constraints()->pixel_format().has_value());
    ASSERT_EQ(buffer_collection_info.settings()->image_format_constraints()->pixel_format(),
              fuchsia_images2::PixelFormat::kNv12);
    ASSERT_TRUE(
        buffer_collection_info.settings()->image_format_constraints()->color_spaces().has_value());
    ASSERT_EQ(buffer_collection_info.settings()->image_format_constraints()->color_spaces()->size(),
              1);
    ASSERT_EQ(buffer_collection_info.settings()->image_format_constraints()->color_spaces()->at(0),
              fuchsia_images2::ColorSpace::kRec709);
  };

  check_allocation_results(collection_1->WaitForAllBuffersAllocated());
  check_allocation_results(collection_2->WaitForAllBuffersAllocated());
}

// Regression-avoidance test for fxbug.dev/60895:
//  * One client with two NV12 ImageFormatConstraints, one with rec601, one with rec709
//  * One client with NV12 ImageFormatConstraints with rec709.
//  * Scrambled ordering of which constraints get processed first, but deterministically check each
//    client going first in the first couple iterations.
TEST(Sysmem,
     MultipleParticipants_TwoImageFormatConstraintsSamePixelFormat_CompatibleColorspacesV2) {
  // Multiple iterations to try to repro fxbug.dev/60895, in case it comes back.  This should be
  // at least 2 to check both orderings with two clients.
  std::atomic<uint32_t> clean_failure_seen_count = 0;
  const uint32_t kCleanFailureSeenGoal = 15;
  const uint32_t kMaxIterations = 50;
  uint32_t iteration;
  for (iteration = 0;
       iteration < kMaxIterations && clean_failure_seen_count.load() < kCleanFailureSeenGoal;
       ++iteration) {
    if (iteration % 100 == 0) {
      printf("starting iteration: %u clean_failure_seen_count: %u\n", iteration,
             clean_failure_seen_count.load());
    }
    const uint32_t kNumClients = 2;
    std::vector<fidl::SyncClient<v2::BufferCollection>> clients = create_clients_v2(kNumClients);

    auto build_constraints = [](bool include_rec601) -> v2::BufferCollectionConstraints {
      v2::BufferCollectionConstraints constraints;
      constraints.usage().emplace();
      constraints.usage()->cpu() =
          fuchsia_sysmem::wire::kCpuUsageReadOften | fuchsia_sysmem::wire::kCpuUsageWriteOften;
      constraints.min_buffer_count_for_camping() = 1;
      constraints.buffer_memory_constraints().emplace();
      auto& buffer_memory = constraints.buffer_memory_constraints().value();
      buffer_memory.min_size_bytes() = zx_system_get_page_size();
      buffer_memory.cpu_domain_supported() = true;
      ZX_ASSERT(!buffer_memory.max_size_bytes().has_value());
      constraints.image_format_constraints().emplace(1 + (include_rec601 ? 1 : 0));
      uint32_t image_constraints_index = 0;
      if (include_rec601) {
        auto& image_constraints =
            constraints.image_format_constraints()->at(image_constraints_index);
        image_constraints.pixel_format() = fuchsia_images2::PixelFormat::kNv12;
        image_constraints.color_spaces().emplace(1);
        image_constraints.color_spaces()->at(0) = fuchsia_images2::ColorSpace::kRec601Ntsc;
        ++image_constraints_index;
      }
      auto& image_constraints = constraints.image_format_constraints()->at(image_constraints_index);
      image_constraints.pixel_format() = fuchsia_images2::PixelFormat::kNv12;
      image_constraints.color_spaces().emplace(1);
      image_constraints.color_spaces()->at(0) = fuchsia_images2::ColorSpace::kRec709;
      return constraints;
    };

    // Both collections should yield the same results
    auto check_allocation_results =
        [&](const fidl::Result<v2::BufferCollection::WaitForAllBuffersAllocated>&
                allocation_result) {
          EXPECT_FALSE(allocation_result.is_ok());
          EXPECT_TRUE(allocation_result.error_value().is_domain_error());
          EXPECT_STATUS(allocation_result.error_value().domain_error(), ZX_ERR_NOT_SUPPORTED);
          if (allocation_result.is_error() && allocation_result.error_value().is_domain_error() &&
              allocation_result.error_value().domain_error() == ZX_ERR_NOT_SUPPORTED) {
            ++clean_failure_seen_count;
          }
        };

    std::vector<std::thread> client_threads;
    std::atomic<uint32_t> ready_thread_count = 0;
    std::atomic<bool> step_0 = false;
    for (uint32_t i = 0; i < kNumClients; ++i) {
      client_threads.emplace_back([&, which_client = i] {
        std::random_device random_device;
        std::uint_fast64_t seed{random_device()};
        std::mt19937_64 prng{seed};
        std::uniform_int_distribution<uint32_t> uint32_distribution(
            0, std::numeric_limits<uint32_t>::max());
        ++ready_thread_count;
        while (!step_0.load()) {
          // spin-wait
        }
        // randomize the continuation timing
        zx::nanosleep(zx::deadline_after(zx::usec(uint32_distribution(prng) % 50)));
        v2::BufferCollectionSetConstraintsRequest set_constraints_request;
        set_constraints_request.constraints() = build_constraints(which_client % 2 == 0);
        auto set_constraints_result =
            clients[which_client]->SetConstraints(std::move(set_constraints_request));
        if (!set_constraints_result.is_ok()) {
          EXPECT_STATUS(set_constraints_result.error_value().status(), ZX_ERR_PEER_CLOSED);
          return;
        }
        // randomize the continuation timing
        zx::nanosleep(zx::deadline_after(zx::usec(uint32_distribution(prng) % 50)));
        auto wait_result = clients[which_client]->WaitForAllBuffersAllocated();
        EXPECT_FALSE(wait_result.is_ok());
        if (!wait_result.is_ok() && wait_result.error_value().is_framework_error()) {
          EXPECT_STATUS(wait_result.error_value().framework_error().status(), ZX_ERR_PEER_CLOSED);
          return;
        }
        check_allocation_results(wait_result);
      });
    }

    while (ready_thread_count.load() != kNumClients) {
      // spin wait
    }

    step_0.store(true);

    for (auto& thread : client_threads) {
      thread.join();
    }
  }

  printf("iterations: %u clean_failure_seen_count: %u\n", iteration,
         clean_failure_seen_count.load());
}

TEST(Sysmem, DuplicateSyncV2) {
  auto allocator = connect_to_sysmem_driver_v2();

  auto token_endpoints_1 = fidl::CreateEndpoints<v2::BufferCollectionToken>();
  ASSERT_TRUE(token_endpoints_1.is_ok());
  auto [token_client_1, token_server_1] = std::move(*token_endpoints_1);
  fidl::SyncClient token_1{std::move(token_client_1)};

  v2::AllocatorAllocateSharedCollectionRequest allocate_non_shared_request;
  allocate_non_shared_request.token_request() = std::move(token_server_1);
  ASSERT_TRUE(allocator->AllocateSharedCollection(std::move(allocate_non_shared_request)).is_ok());

  std::vector<zx_rights_t> rights_attenuation_masks{ZX_RIGHT_SAME_RIGHTS};
  v2::BufferCollectionTokenDuplicateSyncRequest duplicate_sync_request;
  duplicate_sync_request.rights_attenuation_masks() = std::move(rights_attenuation_masks);
  auto duplicate_result = token_1->DuplicateSync(std::move(duplicate_sync_request));
  ASSERT_TRUE(duplicate_result.is_ok());
  ASSERT_TRUE(duplicate_result->tokens().has_value());
  ASSERT_EQ(duplicate_result->tokens()->size(), 1);
  auto token_client_2 = std::move(duplicate_result->tokens()->at(0));

  auto collection_endpoints_1 = fidl::CreateEndpoints<v2::BufferCollection>();
  ASSERT_OK(collection_endpoints_1);
  auto [collection_client_1, collection_server_1] = std::move(*collection_endpoints_1);
  fidl::SyncClient collection_1{std::move(collection_client_1)};

  ASSERT_NE(token_1.client_end().channel().get(), ZX_HANDLE_INVALID);

  v2::AllocatorBindSharedCollectionRequest bind_shared_request;
  bind_shared_request.token() = token_1.TakeClientEnd();
  bind_shared_request.buffer_collection_request() = std::move(collection_server_1);
  ASSERT_TRUE(allocator->BindSharedCollection(std::move(bind_shared_request)).is_ok());

  SetDefaultCollectionNameV2(collection_1);

  v2::BufferCollectionConstraints constraints_1;
  constraints_1.usage().emplace();
  constraints_1.usage()->cpu() =
      fuchsia_sysmem::wire::kCpuUsageReadOften | fuchsia_sysmem::wire::kCpuUsageWriteOften;
  constraints_1.min_buffer_count_for_camping() = 1;
  constraints_1.buffer_memory_constraints().emplace();
  auto& buffer_memory = constraints_1.buffer_memory_constraints().value();
  buffer_memory.min_size_bytes() = 64 * 1024;
  buffer_memory.cpu_domain_supported() = true;

  // Start with constraints_2 a copy of constraints_1.  There are no handles
  // in the constraints struct so a struct copy instead of clone is fine here.
  v2::BufferCollectionConstraints constraints_2(constraints_1);
  v2::BufferCollectionConstraints constraints_3(constraints_1);
  v2::BufferCollectionConstraints constraints_4(constraints_1);

  v2::BufferCollectionSetConstraintsRequest set_constraints_request;
  set_constraints_request.constraints() = std::move(constraints_1);
  ASSERT_TRUE(collection_1->SetConstraints(std::move(set_constraints_request)).is_ok());

  auto collection_endpoints_2 = fidl::CreateEndpoints<v2::BufferCollection>();
  ASSERT_OK(collection_endpoints_2);
  fidl::SyncClient collection_2{std::move(collection_endpoints_2->client)};

  fidl::SyncClient token_2{std::move(token_client_2)};
  // Remove write from last token
  std::vector<zx_rights_t> rights_attenuation_masks_2{ZX_RIGHT_SAME_RIGHTS,
                                                      ZX_DEFAULT_VMO_RIGHTS & ~ZX_RIGHT_WRITE};
  v2::BufferCollectionTokenDuplicateSyncRequest duplicate_sync_request2;
  duplicate_sync_request2.rights_attenuation_masks() = std::move(rights_attenuation_masks_2);
  auto duplicate_result_2 = token_2->DuplicateSync(std::move(duplicate_sync_request2));
  ASSERT_TRUE(duplicate_result_2.is_ok());
  ASSERT_TRUE(duplicate_result_2->tokens().has_value());
  ASSERT_EQ(duplicate_result_2->tokens()->size(), 2);

  ASSERT_NE(token_2.client_end().channel().get(), ZX_HANDLE_INVALID);

  v2::AllocatorBindSharedCollectionRequest bind_shared_request2;
  bind_shared_request2.token() = token_2.TakeClientEnd();
  bind_shared_request2.buffer_collection_request() = std::move(collection_endpoints_2->server);
  ASSERT_TRUE(allocator->BindSharedCollection(std::move(bind_shared_request2)).is_ok());

  v2::BufferCollectionSetConstraintsRequest set_constraints_request2;
  set_constraints_request2.constraints() = std::move(constraints_2);
  ASSERT_TRUE(collection_2->SetConstraints(std::move(set_constraints_request2)).is_ok());

  auto collection_endpoints_3 = fidl::CreateEndpoints<v2::BufferCollection>();
  ASSERT_TRUE(collection_endpoints_3.is_ok());
  fidl::SyncClient collection_3{std::move(collection_endpoints_3->client)};

  auto collection_endpoints_4 = fidl::CreateEndpoints<v2::BufferCollection>();
  ASSERT_TRUE(collection_endpoints_4.is_ok());
  fidl::SyncClient collection_4{std::move(collection_endpoints_4->client)};

  ASSERT_TRUE(duplicate_result_2->tokens().has_value());
  ASSERT_NE(duplicate_result_2->tokens()->at(0).channel().get(), ZX_HANDLE_INVALID);

  v2::AllocatorBindSharedCollectionRequest bind_shared_request3;
  bind_shared_request3.token() = std::move(duplicate_result_2->tokens()->at(0));
  bind_shared_request3.buffer_collection_request() = std::move(collection_endpoints_3->server);
  ASSERT_TRUE(allocator->BindSharedCollection(std::move(bind_shared_request3)).is_ok());

  ASSERT_NE(duplicate_result_2->tokens()->at(1).channel().get(), ZX_HANDLE_INVALID);

  v2::AllocatorBindSharedCollectionRequest bind_shared_request4;
  bind_shared_request4.token() = std::move(duplicate_result_2->tokens()->at(1));
  bind_shared_request4.buffer_collection_request() = std::move(collection_endpoints_4->server);
  ASSERT_TRUE(allocator->BindSharedCollection(std::move(bind_shared_request4)).is_ok());

  v2::BufferCollectionSetConstraintsRequest set_constraints_request3;
  set_constraints_request3.constraints() = std::move(constraints_3);
  ASSERT_TRUE(collection_3->SetConstraints(std::move(set_constraints_request3)).is_ok());

  v2::BufferCollectionSetConstraintsRequest set_constraints_request4;
  set_constraints_request4.constraints() = std::move(constraints_4);
  ASSERT_TRUE(collection_4->SetConstraints(std::move(set_constraints_request4)).is_ok());

  //
  // Only after all participants have SetConstraints() will
  // the allocation be successful.
  //
  auto allocate_result_1 = collection_1->WaitForAllBuffersAllocated();
  // This is the first round-trip to/from sysmem.  A failure here can be due
  // to any step above failing async.
  ASSERT_TRUE(allocate_result_1.is_ok());

  auto allocate_result_3 = collection_3->WaitForAllBuffersAllocated();
  ASSERT_TRUE(allocate_result_3.is_ok());

  auto allocate_result_4 = collection_4->WaitForAllBuffersAllocated();
  ASSERT_TRUE(allocate_result_4.is_ok());

  // Check rights
  zx_info_handle_basic_t info = {};
  ASSERT_OK(allocate_result_3->buffer_collection_info()->buffers()->at(0).vmo()->get_info(
      ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr));
  ASSERT_EQ(info.rights & ZX_RIGHT_WRITE, ZX_RIGHT_WRITE);

  ASSERT_OK(allocate_result_4->buffer_collection_info()->buffers()->at(0).vmo()->get_info(
      ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr));
  ASSERT_EQ(info.rights & ZX_RIGHT_WRITE, 0);
}

TEST(Sysmem, CloseWithOutstandingWaitV2) {
  auto allocator_1 = connect_to_sysmem_driver_v2();
  ASSERT_TRUE(allocator_1.is_ok());

  auto token_endpoints_1 = fidl::CreateEndpoints<v2::BufferCollectionToken>();
  ASSERT_TRUE(token_endpoints_1.is_ok());
  auto [token_client_1, token_server_1] = std::move(*token_endpoints_1);
  fidl::SyncClient token_1{std::move(token_client_1)};

  v2::AllocatorAllocateSharedCollectionRequest allocate_shared_request;
  allocate_shared_request.token_request() = std::move(token_server_1);
  ASSERT_TRUE(allocator_1->AllocateSharedCollection(std::move(allocate_shared_request)).is_ok());

  auto token_endpoints_2 = fidl::CreateEndpoints<v2::BufferCollectionToken>();
  ASSERT_TRUE(token_endpoints_2.is_ok());
  auto [token_client_2, token_server_2] = std::move(*token_endpoints_2);

  v2::BufferCollectionTokenDuplicateRequest duplicate_request;
  duplicate_request.rights_attenuation_mask() = ZX_RIGHT_SAME_RIGHTS;
  duplicate_request.token_request() = std::move(token_server_2);
  ASSERT_TRUE(token_1->Duplicate(std::move(duplicate_request)).is_ok());

  auto collection_endpoints_1 = fidl::CreateEndpoints<v2::BufferCollection>();
  ASSERT_OK(collection_endpoints_1);
  auto [collection_client_1, collection_server_1] = std::move(*collection_endpoints_1);
  fidl::SyncClient collection_1{std::move(collection_client_1)};

  ASSERT_NE(token_1.client_end().channel().get(), ZX_HANDLE_INVALID);

  v2::AllocatorBindSharedCollectionRequest bind_shared_request;
  bind_shared_request.token() = token_1.TakeClientEnd();
  bind_shared_request.buffer_collection_request() = std::move(collection_server_1);
  ASSERT_TRUE(allocator_1->BindSharedCollection(std::move(bind_shared_request)).is_ok());

  v2::BufferCollectionConstraints constraints_1;
  constraints_1.usage().emplace();
  constraints_1.usage()->cpu() =
      fuchsia_sysmem::wire::kCpuUsageReadOften | fuchsia_sysmem::wire::kCpuUsageWriteOften;
  constraints_1.min_buffer_count_for_camping() = 1;

  constraints_1.image_format_constraints().emplace(1);

  auto& image_constraints_1 = constraints_1.image_format_constraints()->at(0);

  image_constraints_1.pixel_format() = fuchsia_images2::PixelFormat::kR8G8B8A8;
  image_constraints_1.pixel_format_modifier() = fuchsia_images2::kFormatModifierLinear;
  image_constraints_1.color_spaces().emplace(1);
  image_constraints_1.color_spaces()->at(0) = fuchsia_images2::ColorSpace::kSrgb;

  v2::BufferCollectionSetConstraintsRequest set_constraints_request;
  set_constraints_request.constraints() = std::move(constraints_1);
  ASSERT_TRUE(collection_1->SetConstraints(std::move(set_constraints_request)).is_ok());

  std::thread wait_thread([&collection_1]() {
    auto allocate_result_1 = collection_1->WaitForAllBuffersAllocated();
    EXPECT_TRUE(allocate_result_1.is_error());
    EXPECT_TRUE(allocate_result_1.error_value().is_framework_error());
    EXPECT_EQ(allocate_result_1.error_value().framework_error().status(), ZX_ERR_PEER_CLOSED);
  });

  // Try to wait until the wait has been processed by the server.
  zx_nanosleep(zx_deadline_after(ZX_SEC(5)));

  auto collection_endpoints_2 = fidl::CreateEndpoints<v2::BufferCollection>();
  ASSERT_TRUE(collection_endpoints_2.is_ok());
  auto [collection_client_2, collection_server_2] = std::move(*collection_endpoints_2);
  fidl::SyncClient collection_2{std::move(collection_client_2)};

  ASSERT_TRUE(collection_1->Sync().is_ok());

  ASSERT_NE(token_client_2.channel().get(), ZX_HANDLE_INVALID);

  v2::AllocatorBindSharedCollectionRequest bind_shared_request2;
  bind_shared_request2.token() = std::move(token_client_2);
  bind_shared_request2.buffer_collection_request() = std::move(collection_server_2);
  ASSERT_TRUE(allocator_1->BindSharedCollection(std::move(bind_shared_request2)).is_ok());

  collection_2 = {};

  wait_thread.join();

  ASSERT_OK(verify_connectivity_v2(*allocator_1));
}

TEST(Sysmem, ConstraintsRetainedBeyondCleanCloseV2) {
  auto allocator = connect_to_sysmem_driver_v2();

  auto token_endpoints_1 = fidl::CreateEndpoints<v2::BufferCollectionToken>();
  ASSERT_TRUE(token_endpoints_1.is_ok());
  auto [token_client_1, token_server_1] = std::move(*token_endpoints_1);
  fidl::SyncClient token_1{std::move(token_client_1)};

  // Client 1 creates a token and new LogicalBufferCollection using
  // AllocateSharedCollection().
  v2::AllocatorAllocateSharedCollectionRequest allocate_shared_request;
  allocate_shared_request.token_request() = std::move(token_server_1);
  ASSERT_TRUE(
      allocator->AllocateSharedCollection(std::move(std::move(allocate_shared_request))).is_ok());

  auto token_endpoints_2 = fidl::CreateEndpoints<v2::BufferCollectionToken>();
  ASSERT_TRUE(token_endpoints_2.is_ok());
  auto [token_client_2, token_server_2] = std::move(*token_endpoints_2);

  // Client 1 duplicates its token and gives the duplicate to client 2 (this
  // test is single proc, so both clients are coming from this client
  // process - normally the two clients would be in separate processes with
  // token_client_2 transferred to another participant).
  v2::BufferCollectionTokenDuplicateRequest duplicate_request;
  duplicate_request.rights_attenuation_mask() = ZX_RIGHT_SAME_RIGHTS;
  duplicate_request.token_request() = std::move(token_server_2);
  ASSERT_TRUE(token_1->Duplicate(std::move(duplicate_request)).is_ok());

  auto collection_endpoints_1 = fidl::CreateEndpoints<v2::BufferCollection>();
  ASSERT_TRUE(collection_endpoints_1.is_ok());
  auto [collection_client_1, collection_server_1] = std::move(*collection_endpoints_1);
  fidl::SyncClient collection_1{std::move(collection_client_1)};

  ASSERT_NE(token_1.client_end().channel().get(), ZX_HANDLE_INVALID);

  v2::AllocatorBindSharedCollectionRequest bind_shared_request;
  bind_shared_request.token() = token_1.TakeClientEnd();
  bind_shared_request.buffer_collection_request() = std::move(collection_server_1);
  ASSERT_TRUE(allocator->BindSharedCollection(std::move(bind_shared_request)).is_ok());

  SetDefaultCollectionNameV2(collection_1);

  v2::BufferCollectionConstraints constraints_1;
  constraints_1.usage().emplace();
  constraints_1.usage()->cpu() =
      fuchsia_sysmem::wire::kCpuUsageReadOften | fuchsia_sysmem::wire::kCpuUsageWriteOften;
  constraints_1.min_buffer_count_for_camping() = 2;
  constraints_1.buffer_memory_constraints().emplace();
  auto& buffer_memory = constraints_1.buffer_memory_constraints().value();
  buffer_memory.min_size_bytes() = 64 * 1024;
  buffer_memory.max_size_bytes() = 64 * 1024;
  buffer_memory.physically_contiguous_required() = false;
  buffer_memory.secure_required() = false;
  buffer_memory.ram_domain_supported() = false;
  buffer_memory.cpu_domain_supported() = true;
  buffer_memory.inaccessible_domain_supported() = false;
  ZX_DEBUG_ASSERT(!buffer_memory.heap_permitted().has_value());

  // constraints_2 is just a copy of constraints_1 - since both participants
  // specify min_buffer_count_for_camping 2, the total number of allocated
  // buffers will be 4.  There are no handles in the constraints struct so a
  // struct copy instead of clone is fine here.
  v2::BufferCollectionConstraints constraints_2(constraints_1);
  ASSERT_EQ(constraints_2.min_buffer_count_for_camping(), 2);

  v2::BufferCollectionSetConstraintsRequest set_constraints_request;
  set_constraints_request.constraints() = std::move(constraints_1);
  ASSERT_TRUE(collection_1->SetConstraints(std::move(set_constraints_request)).is_ok());

  // Client 2 connects to sysmem separately.
  auto allocator_2 = connect_to_sysmem_driver_v2();
  ASSERT_TRUE(allocator_2.is_ok());

  auto collection_endpoints_2 = fidl::CreateEndpoints<v2::BufferCollection>();
  ASSERT_TRUE(collection_endpoints_2.is_ok());
  auto [collection_client_2, collection_server_2] = std::move(*collection_endpoints_2);
  fidl::SyncClient collection_2{std::move(collection_client_2)};

  // Just because we can, perform this sync as late as possible, just before
  // the BindSharedCollection() via allocator2_client_2.  Without this Sync(),
  // the BindSharedCollection() might arrive at the server before the
  // Duplicate() that delivered the server end of token_client_2 to sysmem,
  // which would cause sysmem to not recognize the token.
  ASSERT_TRUE(collection_1->Sync().is_ok());

  // client 1 will now do a clean Close(), but client 1's constraints will be
  // retained by the LogicalBufferCollection.
  ASSERT_TRUE(collection_1->Close().is_ok());
  // close client 1's channel.
  collection_1 = {};

  // Wait briefly so that LogicalBufferCollection will have seen the channel
  // closure of client 1 before client 2 sets constraints.  If we wanted to
  // eliminate this sleep we could add a call to query how many
  // BufferCollection views still exist per LogicalBufferCollection, but that
  // call wouldn't be meant to be used by normal clients, so it seems best to
  // avoid adding such a call.
  nanosleep_duration(zx::msec(250));

  ASSERT_NE(token_client_2.channel().get(), ZX_HANDLE_INVALID);

  v2::AllocatorBindSharedCollectionRequest bind_shared_request2;
  bind_shared_request2.token() = std::move(token_client_2);
  bind_shared_request2.buffer_collection_request() = std::move(collection_server_2);
  ASSERT_TRUE(allocator_2->BindSharedCollection(std::move(bind_shared_request2)).is_ok());

  SetDefaultCollectionNameV2(collection_2);

  // Not all constraints have been input (client 2 hasn't SetConstraints()
  // yet), so the buffers haven't been allocated yet.
  auto check_result_2 = collection_2->CheckAllBuffersAllocated();
  ASSERT_FALSE(check_result_2.is_ok());
  ASSERT_TRUE(check_result_2.error_value().is_domain_error());
  EXPECT_EQ(check_result_2.error_value().domain_error(), ZX_ERR_UNAVAILABLE);

  v2::BufferCollectionSetConstraintsRequest set_constraints_request2;
  set_constraints_request2.constraints() = std::move(constraints_2);
  ASSERT_TRUE(collection_2->SetConstraints(std::move(set_constraints_request2)).is_ok());

  //
  // Now that client 2 has SetConstraints(), the allocation will proceed, with
  // client 1's constraints included despite client 1 having done a clean
  // Close().
  //
  auto allocate_result_2 = collection_2->WaitForAllBuffersAllocated();
  ASSERT_TRUE(allocate_result_2.is_ok());

  // The fact that this is 4 instead of 2 proves that client 1's constraints
  // were taken into account.
  ASSERT_EQ(allocate_result_2->buffer_collection_info()->buffers()->size(), 4);
}

TEST(Sysmem, HeapConstraintsV2) {
  auto collection = make_single_participant_collection_v2();

  v2::BufferCollectionConstraints constraints;
  constraints.usage().emplace();
  constraints.usage()->vulkan() = v2::kVulkanUsageTransferDst;
  constraints.min_buffer_count_for_camping() = 1;
  constraints.buffer_memory_constraints().emplace();
  auto& buffer_memory = constraints.buffer_memory_constraints().value();
  buffer_memory.min_size_bytes() = 4 * 1024;
  buffer_memory.max_size_bytes() = 4 * 1024;
  buffer_memory.physically_contiguous_required() = true;
  buffer_memory.secure_required() = false;
  buffer_memory.ram_domain_supported() = false;
  buffer_memory.cpu_domain_supported() = false;
  buffer_memory.inaccessible_domain_supported() = true;
  buffer_memory.heap_permitted() = {v2::HeapType::kSystemRam};

  v2::BufferCollectionSetConstraintsRequest set_constraints_request;
  set_constraints_request.constraints() = std::move(constraints);
  ASSERT_TRUE(collection->SetConstraints(std::move(set_constraints_request)).is_ok());

  auto allocate_result = collection->WaitForAllBuffersAllocated();
  // This is the first round-trip to/from sysmem.  A failure here can be due
  // to any step above failing async.
  ASSERT_TRUE(allocate_result.is_ok());
  ASSERT_EQ(allocate_result->buffer_collection_info()->buffers()->size(), 1);
  ASSERT_EQ(allocate_result->buffer_collection_info()
                ->settings()
                ->buffer_settings()
                ->coherency_domain()
                .value(),
            v2::CoherencyDomain::kInaccessible);
  ASSERT_EQ(
      allocate_result->buffer_collection_info()->settings()->buffer_settings()->heap().value(),
      v2::HeapType::kSystemRam);
  ASSERT_TRUE(allocate_result->buffer_collection_info()
                  ->settings()
                  ->buffer_settings()
                  ->is_physically_contiguous()
                  .value());
}

TEST(Sysmem, CpuUsageAndInaccessibleDomainFailsV2) {
  auto collection = make_single_participant_collection_v2();

  v2::BufferCollectionConstraints constraints;
  constraints.usage().emplace();
  constraints.usage()->cpu() =
      fuchsia_sysmem::wire::kCpuUsageReadOften | fuchsia_sysmem::wire::kCpuUsageWriteOften;
  constraints.min_buffer_count_for_camping() = 1;
  constraints.buffer_memory_constraints().emplace();
  auto& buffer_memory = constraints.buffer_memory_constraints().value();
  buffer_memory.min_size_bytes() = 4 * 1024;
  buffer_memory.max_size_bytes() = 4 * 1024;
  buffer_memory.physically_contiguous_required() = true;
  buffer_memory.secure_required() = false;
  buffer_memory.ram_domain_supported() = false;
  buffer_memory.cpu_domain_supported() = false;
  buffer_memory.inaccessible_domain_supported() = true;
  buffer_memory.heap_permitted() = {v2::HeapType::kSystemRam};

  v2::BufferCollectionSetConstraintsRequest set_constraints_request;
  set_constraints_request.constraints() = std::move(constraints);
  ASSERT_TRUE(collection->SetConstraints(std::move(set_constraints_request)).is_ok());

  auto allocate_result = collection->WaitForAllBuffersAllocated();
  // usage.cpu != 0 && inaccessible_domain_supported is expected to result in failure to
  // allocate.
  ASSERT_FALSE(allocate_result.is_ok());
}

TEST(Sysmem, SystemRamHeapSupportsAllDomainsV2) {
  v2::CoherencyDomain domains[] = {
      v2::CoherencyDomain::kCpu,
      v2::CoherencyDomain::kRam,
      v2::CoherencyDomain::kInaccessible,
  };

  for (const v2::CoherencyDomain domain : domains) {
    auto collection = make_single_participant_collection_v2();

    v2::BufferCollectionConstraints constraints;
    constraints.usage().emplace();
    constraints.usage()->vulkan() = v2::kVulkanImageUsageTransferDst;
    constraints.min_buffer_count_for_camping() = 1;
    constraints.buffer_memory_constraints().emplace();
    auto& buffer_memory = constraints.buffer_memory_constraints().value();
    buffer_memory.min_size_bytes() = 4 * 1024;
    buffer_memory.max_size_bytes() = 4 * 1024;
    buffer_memory.physically_contiguous_required() = true;
    buffer_memory.secure_required() = false;
    buffer_memory.ram_domain_supported() = (domain == v2::CoherencyDomain::kRam);
    buffer_memory.cpu_domain_supported() = (domain == v2::CoherencyDomain::kCpu);
    buffer_memory.inaccessible_domain_supported() = (domain == v2::CoherencyDomain::kInaccessible);
    buffer_memory.heap_permitted() = {v2::HeapType::kSystemRam};

    v2::BufferCollectionSetConstraintsRequest set_constraints_request;
    set_constraints_request.constraints() = std::move(constraints);
    ASSERT_TRUE(collection->SetConstraints(std::move(set_constraints_request)).is_ok());

    auto allocate_result = collection->WaitForAllBuffersAllocated();
    ASSERT_TRUE(allocate_result.is_ok(), "Failed Allocate(): domain supported = %u",
                static_cast<uint32_t>(domain));

    ASSERT_EQ(domain,
              allocate_result->buffer_collection_info()
                  ->settings()
                  ->buffer_settings()
                  ->coherency_domain()
                  .value(),
              "Coherency domain doesn't match constraints");
  }
}

TEST(Sysmem, RequiredSizeV2) {
  auto collection = make_single_participant_collection_v2();

  v2::BufferCollectionConstraints constraints;
  constraints.usage().emplace();
  constraints.usage()->cpu() =
      fuchsia_sysmem::wire::kCpuUsageReadOften | fuchsia_sysmem::wire::kCpuUsageWriteOften;
  constraints.min_buffer_count_for_camping() = 1;
  ZX_DEBUG_ASSERT(!constraints.buffer_memory_constraints().has_value());
  constraints.image_format_constraints().emplace(1);
  auto& image_constraints = constraints.image_format_constraints()->at(0);
  image_constraints.pixel_format() = fuchsia_images2::PixelFormat::kNv12;
  image_constraints.color_spaces().emplace(1);
  image_constraints.color_spaces()->at(0) = fuchsia_images2::ColorSpace::kRec709;
  image_constraints.min_size() = {256, 256};
  image_constraints.max_size() = {std::numeric_limits<uint32_t>::max(),
                                  std::numeric_limits<uint32_t>::max()};
  image_constraints.min_bytes_per_row() = 256;
  image_constraints.max_bytes_per_row() = std::numeric_limits<uint32_t>::max();
  image_constraints.max_surface_width_times_surface_height() = std::numeric_limits<uint32_t>::max();
  image_constraints.size_alignment() = {1, 1};
  image_constraints.bytes_per_row_divisor() = 1;
  image_constraints.start_offset_divisor() = 1;
  image_constraints.display_rect_alignment() = {1, 1};
  image_constraints.required_max_size() = {512, 1024};

  v2::BufferCollectionSetConstraintsRequest set_constraints_request;
  set_constraints_request.constraints() = std::move(constraints);
  ASSERT_TRUE(collection->SetConstraints(std::move(set_constraints_request)).is_ok());

  auto allocate_result = collection->WaitForAllBuffersAllocated();
  ASSERT_TRUE(allocate_result.is_ok());

  size_t vmo_size;
  zx_status_t status = zx_vmo_get_size(
      allocate_result->buffer_collection_info()->buffers()->at(0).vmo()->get(), &vmo_size);
  ASSERT_EQ(status, ZX_OK);

  // Image must be at least 512x1024 NV12, due to the required max sizes
  // above.
  EXPECT_LE(1024 * 512 * 3 / 2, vmo_size);
}

TEST(Sysmem, CpuUsageAndNoBufferMemoryConstraintsV2) {
  auto allocator_1 = connect_to_sysmem_driver_v2();
  ASSERT_TRUE(allocator_1.is_ok());

  auto token_endpoints_1 = fidl::CreateEndpoints<v2::BufferCollectionToken>();
  ASSERT_TRUE(token_endpoints_1.is_ok());
  auto [token_client_1, token_server_1] = std::move(*token_endpoints_1);
  fidl::SyncClient token_1{std::move(token_client_1)};

  v2::AllocatorAllocateSharedCollectionRequest allocate_shared_request;
  allocate_shared_request.token_request() = std::move(token_server_1);
  ASSERT_TRUE(allocator_1->AllocateSharedCollection(std::move(allocate_shared_request)).is_ok());

  auto token_endpoints_2 = fidl::CreateEndpoints<v2::BufferCollectionToken>();
  ASSERT_TRUE(token_endpoints_2.is_ok());
  auto [token_client_2, token_server_2] = std::move(*token_endpoints_2);

  v2::BufferCollectionTokenDuplicateRequest duplicate_request;
  duplicate_request.rights_attenuation_mask() = ZX_RIGHT_SAME_RIGHTS;
  duplicate_request.token_request() = std::move(token_server_2);
  ASSERT_TRUE(token_1->Duplicate(std::move(duplicate_request)).is_ok());

  auto collection_endpoints_1 = fidl::CreateEndpoints<v2::BufferCollection>();
  ASSERT_TRUE(collection_endpoints_1.is_ok());
  auto [collection_client_1, collection_server_1] = std::move(*collection_endpoints_1);
  fidl::SyncClient collection_1{std::move(collection_client_1)};

  ASSERT_NE(token_1.client_end().channel().get(), ZX_HANDLE_INVALID);

  v2::AllocatorBindSharedCollectionRequest bind_shared_request;
  bind_shared_request.token() = token_1.TakeClientEnd();
  bind_shared_request.buffer_collection_request() = std::move(collection_server_1);
  ASSERT_TRUE(allocator_1->BindSharedCollection(std::move(bind_shared_request)).is_ok());

  SetDefaultCollectionNameV2(collection_1);

  // First client has CPU usage constraints but no buffer memory constraints.
  v2::BufferCollectionConstraints constraints_1;
  constraints_1.usage().emplace();
  constraints_1.usage()->cpu() =
      fuchsia_sysmem::wire::kCpuUsageReadOften | fuchsia_sysmem::wire::kCpuUsageWriteOften;
  constraints_1.min_buffer_count_for_camping() = 1;
  ZX_DEBUG_ASSERT(!constraints_1.buffer_memory_constraints().has_value());

  v2::BufferCollectionConstraints constraints_2;
  constraints_2.usage().emplace();
  constraints_2.usage()->display() = v2::kDisplayUsageLayer;
  constraints_2.min_buffer_count_for_camping() = 1;
  constraints_2.buffer_memory_constraints().emplace();
  auto& buffer_memory = constraints_2.buffer_memory_constraints().value();
  buffer_memory.min_size_bytes() =
      1;  // must be at least 1 else no participant has specified min size
  buffer_memory.max_size_bytes() = 0xffffffff;
  buffer_memory.physically_contiguous_required() = false;
  buffer_memory.secure_required() = false;
  buffer_memory.ram_domain_supported() = true;
  buffer_memory.cpu_domain_supported() = true;
  buffer_memory.inaccessible_domain_supported() = true;
  ZX_DEBUG_ASSERT(!buffer_memory.heap_permitted().has_value());

  v2::BufferCollectionSetConstraintsRequest set_constraints_request;
  set_constraints_request.constraints() = std::move(constraints_1);
  ASSERT_TRUE(collection_1->SetConstraints(std::move(set_constraints_request)).is_ok());

  auto allocator_2 = connect_to_sysmem_driver_v2();
  ASSERT_TRUE(allocator_2.is_ok());

  auto collection_endpoints_2 = fidl::CreateEndpoints<v2::BufferCollection>();
  ASSERT_TRUE(collection_endpoints_2.is_ok());
  auto [collection_client_2, collection_server_2] = std::move(*collection_endpoints_2);
  fidl::SyncClient collection_2{std::move(collection_client_2)};

  ASSERT_TRUE(collection_1->Sync().is_ok());

  ASSERT_NE(token_client_2.channel().get(), ZX_HANDLE_INVALID);

  v2::AllocatorBindSharedCollectionRequest bind_shared_request2;
  bind_shared_request2.token() = std::move(token_client_2);
  bind_shared_request2.buffer_collection_request() = std::move(collection_server_2);
  ASSERT_TRUE(allocator_2->BindSharedCollection(std::move(bind_shared_request2)).is_ok());

  v2::BufferCollectionSetConstraintsRequest set_constraints_request2;
  set_constraints_request2.constraints() = std::move(constraints_2);
  ASSERT_TRUE(collection_2->SetConstraints(std::move(set_constraints_request2)).is_ok());

  auto allocate_result_1 = collection_1->WaitForAllBuffersAllocated();
  ASSERT_TRUE(allocate_result_1.is_ok());
  ASSERT_EQ(allocate_result_1->buffer_collection_info()
                ->settings()
                ->buffer_settings()
                ->coherency_domain()
                .value(),
            v2::CoherencyDomain::kCpu);
}

TEST(Sysmem, ContiguousSystemRamIsCachedV2) {
  auto collection = make_single_participant_collection_v2();

  v2::BufferCollectionConstraints constraints;
  constraints.usage().emplace();
  constraints.usage()->vulkan() = v2::kVulkanUsageTransferDst;
  constraints.min_buffer_count_for_camping() = 1;
  constraints.buffer_memory_constraints().emplace();
  auto& buffer_memory = constraints.buffer_memory_constraints().value();
  buffer_memory.min_size_bytes() = 4 * 1024;
  buffer_memory.max_size_bytes() = 4 * 1024;
  buffer_memory.physically_contiguous_required() = true;
  buffer_memory.secure_required() = false;
  buffer_memory.ram_domain_supported() = false;
  buffer_memory.cpu_domain_supported() = true;
  buffer_memory.inaccessible_domain_supported() = false;
  // Constraining this to SYSTEM_RAM is redundant for now.
  buffer_memory.heap_permitted() = {v2::HeapType::kSystemRam};

  v2::BufferCollectionSetConstraintsRequest set_constraints_request;
  set_constraints_request.constraints() = std::move(constraints);
  ASSERT_TRUE(collection->SetConstraints(std::move(set_constraints_request)).is_ok());

  auto allocate_result = collection->WaitForAllBuffersAllocated();
  // This is the first round-trip to/from sysmem.  A failure here can be due
  // to any step above failing async.
  ASSERT_TRUE(allocate_result.is_ok());
  ASSERT_EQ(allocate_result->buffer_collection_info()->buffers()->size(), 1);
  ASSERT_EQ(allocate_result->buffer_collection_info()
                ->settings()
                ->buffer_settings()
                ->coherency_domain()
                .value(),
            v2::CoherencyDomain::kCpu);
  ASSERT_EQ(
      allocate_result->buffer_collection_info()->settings()->buffer_settings()->heap().value(),
      v2::HeapType::kSystemRam);
  ASSERT_TRUE(allocate_result->buffer_collection_info()
                  ->settings()
                  ->buffer_settings()
                  ->is_physically_contiguous()
                  .value());

  // We could potentially map and try some non-aligned accesses, but on x64
  // that'd just work anyway IIRC, so just directly check if the cache policy
  // is cached so that non-aligned accesses will work on aarch64.
  //
  // We're intentionally only requiring this to be true in a test that
  // specifies CoherencyDomain::kCpu - intentionally don't care for
  // CoherencyDomain::kRam or CoherencyDomain::kInaccessible (when not protected).
  // CoherencyDomain::kInaccessible + protected has a separate test (
  // test_sysmem_protected_ram_is_uncached).
  zx_info_vmo_t vmo_info{};
  zx_status_t status = allocate_result->buffer_collection_info()->buffers()->at(0).vmo()->get_info(
      ZX_INFO_VMO, &vmo_info, sizeof(vmo_info), nullptr, nullptr);
  ASSERT_EQ(status, ZX_OK);
  ASSERT_EQ(vmo_info.cache_policy, ZX_CACHE_POLICY_CACHED);
}

TEST(Sysmem, ContiguousSystemRamIsRecycledV2) {
  // This needs to be larger than RAM, to know that this test is really checking if the allocations
  // are being recycled, regardless of what allocation strategy sysmem might be using.
  //
  // Unfortunately, at least under QEMU, allocating zx_system_get_physmem() * 2 takes longer than
  // the test watchdog, so instead of timing out, we early out with printf and fake "success" if
  // that happens.
  //
  // This test currently relies on timeliness/ordering of the ZX_VMO_ZERO_CHILDREN signal and
  // notification to sysmem of that signal vs. allocation of more BufferCollection(s), which to some
  // extent could be viewed as an invalid thing to depend on, but on the other hand, if those
  // mechanisms _are_ delayed too much, in practice we might have problems, so ... for now the test
  // is not ashamed to be relying on that.
  uint64_t total_bytes_to_allocate = zx_system_get_physmem() * 2;
  uint64_t total_bytes_allocated = 0;
  constexpr uint64_t kBytesToAllocatePerPass = 2 * 1024 * 1024;
  zx::time deadline_time = zx::deadline_after(zx::sec(10));
  int64_t iteration_count = 0;
  zx::time start_time = zx::clock::get_monotonic();
  while (total_bytes_allocated < total_bytes_to_allocate) {
    if (zx::clock::get_monotonic() > deadline_time) {
      // Otherwise, we'd potentially trigger the test watchdog.  So far we've only seen this happen
      // in QEMU environments.
      printf(
          "\ntest_sysmem_contiguous_system_ram_is_recycled() internal timeout - fake success - "
          "total_bytes_allocated so far: %zu\n",
          total_bytes_allocated);
      break;
    }

    auto collection = make_single_participant_collection_v2();

    v2::BufferCollectionConstraints constraints;
    constraints.usage().emplace();
    constraints.usage()->vulkan() = v2::kVulkanUsageTransferDst;
    constraints.min_buffer_count_for_camping() = 1;
    constraints.buffer_memory_constraints().emplace();
    auto& buffer_memory = constraints.buffer_memory_constraints().value();
    buffer_memory.min_size_bytes() = kBytesToAllocatePerPass;
    buffer_memory.max_size_bytes() = kBytesToAllocatePerPass;
    buffer_memory.physically_contiguous_required() = true;
    buffer_memory.secure_required() = false;
    buffer_memory.ram_domain_supported() = false;
    buffer_memory.cpu_domain_supported() = true;
    buffer_memory.inaccessible_domain_supported() = false;
    // Constraining this to SYSTEM_RAM is redundant for now.
    buffer_memory.heap_permitted() = {v2::HeapType::kSystemRam};

    v2::BufferCollectionSetConstraintsRequest set_constraints_request;
    set_constraints_request.constraints() = std::move(constraints);
    ASSERT_TRUE(collection->SetConstraints(std::move(set_constraints_request)).is_ok());

    auto allocate_result = collection->WaitForAllBuffersAllocated();
    // This is the first round-trip to/from sysmem.  A failure here can be due
    // to any step above failing async.
    ASSERT_TRUE(allocate_result.is_ok());

    ASSERT_EQ(allocate_result->buffer_collection_info()->buffers()->size(), 1);
    ASSERT_EQ(allocate_result->buffer_collection_info()
                  ->settings()
                  ->buffer_settings()
                  ->coherency_domain()
                  .value(),
              v2::CoherencyDomain::kCpu);
    ASSERT_EQ(
        allocate_result->buffer_collection_info()->settings()->buffer_settings()->heap().value(),
        v2::HeapType::kSystemRam);
    ASSERT_TRUE(allocate_result->buffer_collection_info()
                    ->settings()
                    ->buffer_settings()
                    ->is_physically_contiguous()
                    .value());

    total_bytes_allocated += kBytesToAllocatePerPass;

    iteration_count++;

    // ~collection_client and ~buffer_collection_info should recycle the space used by the VMOs for
    // re-use so that more can be allocated.
  }
  zx::time end_time = zx::clock::get_monotonic();
  zx::duration duration_per_iteration = (end_time - start_time) / iteration_count;

  printf("duration_per_iteration: %" PRId64 "us, or %" PRId64 "ms\n",
         duration_per_iteration.to_usecs(), duration_per_iteration.to_msecs());

  if (total_bytes_allocated >= total_bytes_to_allocate) {
    printf("\ntest_sysmem_contiguous_system_ram_is_recycled() real success\n");
  }
}

TEST(Sysmem, OnlyNoneUsageFailsV2) {
  auto collection = make_single_participant_collection_v2();

  v2::BufferCollectionConstraints constraints;
  constraints.usage().emplace();
  constraints.usage()->none() = v2::kNoneUsage;
  constraints.min_buffer_count_for_camping() = 3;
  constraints.min_buffer_count() = 5;
  constraints.buffer_memory_constraints().emplace();
  auto& buffer_memory = constraints.buffer_memory_constraints().value();
  buffer_memory.min_size_bytes() = 64 * 1024;
  buffer_memory.max_size_bytes() = 128 * 1024;
  buffer_memory.physically_contiguous_required() = false;
  buffer_memory.secure_required() = false;
  buffer_memory.ram_domain_supported() = false;
  buffer_memory.cpu_domain_supported() = true;
  buffer_memory.inaccessible_domain_supported() = false;
  ZX_DEBUG_ASSERT(!buffer_memory.heap_permitted().has_value());
  ZX_DEBUG_ASSERT(!constraints.image_format_constraints().has_value());

  v2::BufferCollectionSetConstraintsRequest set_constraints_request;
  set_constraints_request.constraints() = std::move(constraints);
  ASSERT_TRUE(collection->SetConstraints(std::move(set_constraints_request)).is_ok());

  auto allocate_result = collection->WaitForAllBuffersAllocated();
  // This is the first round-trip to/from sysmem.  A failure here can be due
  // to any step above failing async.
  //
  // If the aggregate usage only has "none" usage, allocation should fail.
  // Because we weren't waiting at the time that allocation failed, we don't
  // necessarily get a response from the wait.
  //
  // TODO(dustingreen): Issue async client request to put the wait in flight
  // before the SetConstraints() so we can verify that the wait succeeds but
  // the allocation_status is ZX_ERR_NOT_SUPPORTED.
  ASSERT_FALSE(allocate_result.is_ok());
}

TEST(Sysmem, NoneUsageAndOtherUsageFromSingleParticipantFailsV2) {
  auto collection = make_single_participant_collection_v2();

  const char* kClientName = "TestClient";
  v2::NodeSetDebugClientInfoRequest set_debug_request;
  set_debug_request.name() = kClientName;
  set_debug_request.id() = 6u;
  ASSERT_TRUE(collection->SetDebugClientInfo(std::move(set_debug_request)).is_ok());

  v2::BufferCollectionConstraints constraints;
  constraints.usage().emplace();
  // Specify both "none" and "cpu" usage from a single participant, which will
  // cause allocation failure.
  constraints.usage()->none() = v2::kNoneUsage;
  constraints.usage()->cpu() = v2::kCpuUsageReadOften;
  constraints.min_buffer_count_for_camping() = 3;
  constraints.min_buffer_count() = 5;
  constraints.buffer_memory_constraints().emplace();
  auto& buffer_memory = constraints.buffer_memory_constraints().value();
  buffer_memory.min_size_bytes() = 64 * 1024;
  buffer_memory.max_size_bytes() = 128 * 1024;
  buffer_memory.physically_contiguous_required() = false;
  buffer_memory.secure_required() = false;
  buffer_memory.ram_domain_supported() = false;
  buffer_memory.cpu_domain_supported() = true;
  buffer_memory.inaccessible_domain_supported() = false;
  ZX_DEBUG_ASSERT(!buffer_memory.heap_permitted().has_value());
  ZX_DEBUG_ASSERT(!constraints.image_format_constraints().has_value());

  v2::BufferCollectionSetConstraintsRequest set_constraints_request;
  set_constraints_request.constraints() = std::move(constraints);
  ASSERT_TRUE(collection->SetConstraints(std::move(set_constraints_request)).is_ok());

  auto allocate_result = collection->WaitForAllBuffersAllocated();
  // This is the first round-trip to/from sysmem.  A failure here can be due
  // to any step above failing async.
  //
  // If the aggregate usage has both "none" usage and "cpu" usage from a
  // single participant, allocation should fail.
  //
  // TODO(dustingreen): Issue async client request to put the wait in flight
  // before the SetConstraints() so we can verify that the wait succeeds but
  // the allocation_status is ZX_ERR_NOT_SUPPORTED.
  ASSERT_FALSE(allocate_result.is_ok());
}

TEST(Sysmem, NoneUsageWithSeparateOtherUsageSucceedsV2) {
  auto allocator = connect_to_sysmem_driver_v2();

  auto token_endpoints_1 = fidl::CreateEndpoints<v2::BufferCollectionToken>();
  ASSERT_OK(token_endpoints_1);
  auto [token_client_1, token_server_1] = std::move(*token_endpoints_1);
  fidl::SyncClient token_1{std::move(token_client_1)};

  // Client 1 creates a token and new LogicalBufferCollection using
  // AllocateSharedCollection().
  v2::AllocatorAllocateSharedCollectionRequest allocate_shared_request;
  allocate_shared_request.token_request() = std::move(token_server_1);
  ASSERT_TRUE(allocator->AllocateSharedCollection(std::move(allocate_shared_request)).is_ok());

  auto token_endpoints_2 = fidl::CreateEndpoints<v2::BufferCollectionToken>();
  ASSERT_TRUE(token_endpoints_2.is_ok());
  auto [token_client_2, token_server_2] = std::move(*token_endpoints_2);

  // Client 1 duplicates its token and gives the duplicate to client 2 (this
  // test is single proc, so both clients are coming from this client
  // process - normally the two clients would be in separate processes with
  // token_client_2 transferred to another participant).
  v2::BufferCollectionTokenDuplicateRequest duplicate_request;
  duplicate_request.rights_attenuation_mask() = ZX_RIGHT_SAME_RIGHTS;
  duplicate_request.token_request() = std::move(token_server_2);
  ASSERT_TRUE(token_1->Duplicate(std::move(duplicate_request)).is_ok());

  auto collection_endpoints_1 = fidl::CreateEndpoints<v2::BufferCollection>();
  ASSERT_TRUE(collection_endpoints_1.is_ok());
  auto [collection_client_1, collection_server_1] = std::move(*collection_endpoints_1);
  fidl::SyncClient collection_1{std::move(collection_client_1)};

  ASSERT_NE(token_1.client_end().channel().get(), ZX_HANDLE_INVALID);

  v2::AllocatorBindSharedCollectionRequest bind_shared_request;
  bind_shared_request.token() = token_1.TakeClientEnd();
  bind_shared_request.buffer_collection_request() = std::move(collection_server_1);
  ASSERT_TRUE(allocator->BindSharedCollection(std::move(bind_shared_request)).is_ok());

  SetDefaultCollectionNameV2(collection_1);

  v2::BufferCollectionConstraints constraints_1;
  constraints_1.usage().emplace();
  constraints_1.usage()->none() = v2::kNoneUsage;
  constraints_1.min_buffer_count_for_camping() = 3;
  constraints_1.buffer_memory_constraints().emplace();
  auto& buffer_memory = constraints_1.buffer_memory_constraints().value();
  // This min_size_bytes is intentionally too small to hold the min_coded_width and
  // min_coded_height in NV12
  // format.
  buffer_memory.min_size_bytes() = 64 * 1024;
  // Allow a max that's just large enough to accommodate the size implied
  // by the min frame size and PixelFormat;
  buffer_memory.max_size_bytes() = (512 * 512) * 3 / 2;
  buffer_memory.physically_contiguous_required() = false;
  buffer_memory.secure_required() = false;
  buffer_memory.ram_domain_supported() = false;
  buffer_memory.cpu_domain_supported() = true;
  buffer_memory.inaccessible_domain_supported() = false;
  ZX_DEBUG_ASSERT(!buffer_memory.heap_permitted().has_value());

  // Start with constraints_2 a copy of constraints_1.  There are no handles
  // in the constraints struct so a struct copy instead of clone is fine here.
  v2::BufferCollectionConstraints constraints_2(constraints_1);
  // Modify constraints_2 to set non-"none" usage.
  constraints_2.usage()->none().reset();
  constraints_2.usage()->vulkan() = v2::kVulkanUsageTransferDst;

  v2::BufferCollectionSetConstraintsRequest set_constraints_request;
  set_constraints_request.constraints() = std::move(constraints_1);
  ASSERT_TRUE(collection_1->SetConstraints(std::move(set_constraints_request)).is_ok());

  // Client 2 connects to sysmem separately.
  auto allocator_2 = connect_to_sysmem_driver_v2();
  ASSERT_TRUE(allocator_2.is_ok());

  auto collection_endpoints_2 = fidl::CreateEndpoints<v2::BufferCollection>();
  ASSERT_TRUE(collection_endpoints_2.is_ok());
  auto [collection_client_2, collection_server_2] = std::move(*collection_endpoints_2);
  fidl::SyncClient collection_2{std::move(collection_client_2)};

  // Just because we can, perform this sync as late as possible, just before
  // the BindSharedCollection() via allocator2_client_2.  Without this Sync(),
  // the BindSharedCollection() might arrive at the server before the
  // Duplicate() that delivered the server end of token_client_2 to sysmem,
  // which would cause sysmem to not recognize the token.
  ASSERT_TRUE(collection_1->Sync().is_ok());

  ASSERT_NE(token_client_2.channel().get(), ZX_HANDLE_INVALID);

  v2::AllocatorBindSharedCollectionRequest bind_shared_request2;
  bind_shared_request2.token() = std::move(token_client_2);
  bind_shared_request2.buffer_collection_request() = std::move(collection_server_2);
  ASSERT_TRUE(allocator_2->BindSharedCollection(std::move(bind_shared_request2)).is_ok());

  v2::BufferCollectionSetConstraintsRequest set_constraints_request2;
  set_constraints_request2.constraints() = std::move(constraints_2);
  ASSERT_TRUE(collection_2->SetConstraints(std::move(set_constraints_request2)).is_ok());

  //
  // Only after both participants (both clients) have SetConstraints() will
  // the allocation be successful.
  //
  auto allocate_result_1 = collection_1->WaitForAllBuffersAllocated();
  // This is the first round-trip to/from sysmem.  A failure here can be due
  // to any step above failing async.
  //
  // Success when at least one participant specifies "none" usage and at least
  // one participant specifies a usage other than "none".
  ASSERT_TRUE(allocate_result_1.is_ok());
}

TEST(Sysmem, PixelFormatBgr24V2) {
  constexpr uint32_t kWidth = 600;
  constexpr uint32_t kHeight = 1;
  constexpr uint32_t kStride = kWidth * zbitl::BytesPerPixel(ZBI_PIXEL_FORMAT_RGB_888);
  constexpr uint32_t divisor = 32;
  constexpr uint32_t kStrideAlign = (kStride + divisor - 1) & ~(divisor - 1);

  auto collection = make_single_participant_collection_v2();

  v2::BufferCollectionConstraints constraints;
  constraints.usage().emplace();
  constraints.usage()->cpu() =
      fuchsia_sysmem::wire::kCpuUsageReadOften | fuchsia_sysmem::wire::kCpuUsageWriteOften;
  constraints.min_buffer_count_for_camping() = 3;
  constraints.buffer_memory_constraints().emplace();
  auto& buffer_memory = constraints.buffer_memory_constraints().value();
  buffer_memory.min_size_bytes() = kStride;
  buffer_memory.max_size_bytes() = kStrideAlign;
  buffer_memory.physically_contiguous_required() = false;
  buffer_memory.secure_required() = false;
  buffer_memory.ram_domain_supported() = true;
  buffer_memory.cpu_domain_supported() = true;
  buffer_memory.inaccessible_domain_supported() = false;
  buffer_memory.heap_permitted() = {v2::HeapType::kSystemRam};
  constraints.image_format_constraints().emplace(1);
  auto& image_constraints = constraints.image_format_constraints()->at(0);
  image_constraints.pixel_format() = fuchsia_images2::PixelFormat::kBgr24;
  image_constraints.color_spaces().emplace(1);
  image_constraints.color_spaces()->at(0) = fuchsia_images2::ColorSpace::kSrgb;
  // The min dimensions intentionally imply a min size that's larger than
  // buffer_memory_constraints.min_size_bytes.
  image_constraints.min_size() = {kWidth, kHeight};
  image_constraints.max_size() = {std::numeric_limits<uint32_t>::max(),
                                  std::numeric_limits<uint32_t>::max()};
  image_constraints.min_bytes_per_row() = kStride;
  image_constraints.max_bytes_per_row() = std::numeric_limits<uint32_t>::max();
  image_constraints.max_surface_width_times_surface_height() = std::numeric_limits<uint32_t>::max();
  image_constraints.size_alignment() = {1, 1};
  image_constraints.bytes_per_row_divisor() = divisor;
  image_constraints.start_offset_divisor() = divisor;
  image_constraints.display_rect_alignment() = {1, 1};

  v2::BufferCollectionSetConstraintsRequest set_constraints_request;
  set_constraints_request.constraints() = std::move(constraints);
  ASSERT_TRUE(collection->SetConstraints(std::move(set_constraints_request)).is_ok());

  auto allocate_result = collection->WaitForAllBuffersAllocated();
  // This is the first round-trip to/from sysmem.  A failure here can be due
  // to any step above failing async.
  ASSERT_TRUE(allocate_result.is_ok());

  auto& buffer_collection_info = allocate_result->buffer_collection_info().value();
  ASSERT_EQ(buffer_collection_info.buffers()->size(), 3);
  ASSERT_EQ(buffer_collection_info.settings()->buffer_settings()->size_bytes().value(),
            kStrideAlign);
  ASSERT_FALSE(
      buffer_collection_info.settings()->buffer_settings()->is_physically_contiguous().value());
  ASSERT_FALSE(buffer_collection_info.settings()->buffer_settings()->is_secure().value());
  ASSERT_EQ(buffer_collection_info.settings()->buffer_settings()->coherency_domain().value(),
            v2::CoherencyDomain::kCpu);
  // We specified image_format_constraints so the result must also have
  // image_format_constraints.
  ASSERT_TRUE(buffer_collection_info.settings()->image_format_constraints().has_value());

  ASSERT_EQ(buffer_collection_info.settings()->image_format_constraints()->pixel_format().value(),
            fuchsia_images2::PixelFormat::kBgr24);

  for (uint32_t i = 0; i < buffer_collection_info.buffers()->size(); ++i) {
    ASSERT_NE(buffer_collection_info.buffers()->at(i).vmo()->get(), ZX_HANDLE_INVALID);
    uint64_t size_bytes = 0;
    auto status =
        zx_vmo_get_size(buffer_collection_info.buffers()->at(i).vmo()->get(), &size_bytes);
    ASSERT_EQ(status, ZX_OK);
    // The portion of the VMO the client can use is large enough to hold the min image size,
    // despite the min buffer size being smaller.
    ASSERT_GE(buffer_collection_info.settings()->buffer_settings()->size_bytes().value(),
              kStrideAlign);
    // The vmo has room for the nominal size of the portion of the VMO the client can use.
    ASSERT_LE(buffer_collection_info.buffers()->at(i).vmo_usable_start().value() +
                  buffer_collection_info.settings()->buffer_settings()->size_bytes().value(),
              size_bytes);
  }
}

// Test that closing a token handle that's had Close() called on it doesn't crash sysmem.
TEST(Sysmem, CloseTokenV2) {
  auto allocator = connect_to_sysmem_driver_v2();

  auto token_endpoints_1 = fidl::CreateEndpoints<v2::BufferCollectionToken>();
  ASSERT_TRUE(token_endpoints_1.is_ok());
  auto [token_client_1, token_server_1] = std::move(*token_endpoints_1);
  fidl::SyncClient token_1{std::move(token_client_1)};

  v2::AllocatorAllocateSharedCollectionRequest allocate_shared_request;
  allocate_shared_request.token_request() = std::move(token_server_1);
  ASSERT_TRUE(allocator->AllocateSharedCollection(std::move(allocate_shared_request)).is_ok());

  auto token_endpoints_2 = fidl::CreateEndpoints<v2::BufferCollectionToken>();
  ASSERT_TRUE(token_endpoints_2.is_ok());
  auto [token_client_2, token_server_2] = std::move(*token_endpoints_2);
  fidl::SyncClient token_2{std::move(token_client_2)};

  v2::BufferCollectionTokenDuplicateRequest duplicate_request;
  duplicate_request.rights_attenuation_mask() = ZX_RIGHT_SAME_RIGHTS;
  duplicate_request.token_request() = std::move(token_server_2);
  ASSERT_TRUE(token_1->Duplicate(std::move(duplicate_request)).is_ok());

  ASSERT_TRUE(token_1->Sync().is_ok());
  ASSERT_TRUE(token_1->Close().is_ok());
  token_1 = {};

  // Try to ensure sysmem processes the token closure before the sync.
  zx_nanosleep(zx_deadline_after(ZX_MSEC(5)));

  EXPECT_TRUE(token_2->Sync().is_ok());
}

TEST(Sysmem, HeapAmlogicSecureV2) {
  if (!is_board_with_amlogic_secure()) {
    return;
  }

  for (uint32_t i = 0; i < 64; ++i) {
    auto collection = make_single_participant_collection_v2();

    v2::BufferCollectionConstraints constraints;
    constraints.usage().emplace();
    constraints.usage()->video() = v2::kVideoUsageHwDecoder;
    constexpr uint32_t kBufferCount = 4;
    constraints.min_buffer_count_for_camping() = kBufferCount;
    constraints.buffer_memory_constraints().emplace();
    auto& buffer_memory = constraints.buffer_memory_constraints().value();
    constexpr uint32_t kBufferSizeBytes = 64 * 1024;
    buffer_memory.min_size_bytes() = kBufferSizeBytes;
    buffer_memory.max_size_bytes() = 128 * 1024;
    buffer_memory.physically_contiguous_required() = true;
    buffer_memory.secure_required() = true;
    buffer_memory.ram_domain_supported() = false;
    buffer_memory.cpu_domain_supported() = false;
    buffer_memory.inaccessible_domain_supported() = true;
    buffer_memory.heap_permitted() = {v2::HeapType::kAmlogicSecure};
    ZX_DEBUG_ASSERT(!constraints.image_format_constraints().has_value());

    v2::BufferCollectionSetConstraintsRequest set_constraints_request;
    set_constraints_request.constraints() = std::move(constraints);
    ASSERT_TRUE(collection->SetConstraints(std::move(set_constraints_request)).is_ok());

    auto allocate_result = collection->WaitForAllBuffersAllocated();
    // This is the first round-trip to/from sysmem.  A failure here can be due
    // to any step above failing async.
    ASSERT_TRUE(allocate_result.is_ok());
    auto& buffer_collection_info = allocate_result->buffer_collection_info().value();
    EXPECT_EQ(buffer_collection_info.buffers()->size(), kBufferCount);
    EXPECT_EQ(buffer_collection_info.settings()->buffer_settings()->size_bytes().value(),
              kBufferSizeBytes);
    EXPECT_TRUE(
        buffer_collection_info.settings()->buffer_settings()->is_physically_contiguous().value());
    EXPECT_TRUE(buffer_collection_info.settings()->buffer_settings()->is_secure().value());
    EXPECT_EQ(buffer_collection_info.settings()->buffer_settings()->coherency_domain().value(),
              v2::CoherencyDomain::kInaccessible);
    EXPECT_EQ(buffer_collection_info.settings()->buffer_settings()->heap().value(),
              v2::HeapType::kAmlogicSecure);
    EXPECT_FALSE(buffer_collection_info.settings()->image_format_constraints().has_value());

    for (uint32_t i = 0; i < buffer_collection_info.buffers()->size(); ++i) {
      EXPECT_NE(buffer_collection_info.buffers()->at(i).vmo()->get(), ZX_HANDLE_INVALID);
      uint64_t size_bytes = 0;
      auto status =
          zx_vmo_get_size(buffer_collection_info.buffers()->at(i).vmo()->get(), &size_bytes);
      ASSERT_EQ(status, ZX_OK);
      EXPECT_EQ(size_bytes, kBufferSizeBytes);
    }

    zx::vmo the_vmo = std::move(buffer_collection_info.buffers()->at(0).vmo().value());
    buffer_collection_info.buffers()->at(0).vmo().reset();
    SecureVmoReadTester tester(std::move(the_vmo));
    ASSERT_DEATH(([&] { tester.AttemptReadFromSecure(); }));
    ASSERT_FALSE(tester.IsReadFromSecureAThing());
  }
}

TEST(Sysmem, HeapAmlogicSecureMiniStressV2) {
  if (!is_board_with_amlogic_secure()) {
    return;
  }

  // 256 64 KiB chunks, and well below protected_memory_size, even accounting for fragmentation.
  const uint32_t kBlockSize = 64 * 1024;
  const uint32_t kTotalSizeThreshold = 256 * kBlockSize;

  // For generating different sequences of random ranges, but still being able to easily repro any
  // failure by putting the uint64_t seed inside the {} here.
  static constexpr std::optional<uint64_t> kForcedSeed{};
  std::random_device random_device;
  std::uint_fast64_t seed{kForcedSeed ? *kForcedSeed : random_device()};
  std::mt19937_64 prng{seed};
  std::uniform_int_distribution<uint32_t> op_distribution(0, 104);
  std::uniform_int_distribution<uint32_t> key_distribution(0, std::numeric_limits<uint32_t>::max());
  // Buffers aren't required to be block aligned.
  const uint32_t max_buffer_size = 4 * kBlockSize;
  std::uniform_int_distribution<uint32_t> size_distribution(1, max_buffer_size);

  struct Vmo {
    uint32_t size;
    zx::vmo vmo;
  };
  using BufferMap = std::multimap<uint32_t, Vmo>;
  BufferMap buffers;
  // random enough for this test; buffers at the end of a big gap are more likely to be selected,
  // but that's fine since which buffers those are is also random due to random key when the buffer
  // is added; still, not claiming this is rigorously random
  auto get_random_buffer = [&key_distribution, &prng, &buffers]() -> BufferMap::iterator {
    if (buffers.empty()) {
      return buffers.end();
    }
    uint32_t key = key_distribution(prng);
    auto iter = buffers.upper_bound(key);
    if (iter == buffers.end()) {
      iter = buffers.begin();
    }
    return iter;
  };

  uint32_t total_size = 0;
  auto add = [&key_distribution, &size_distribution, &prng, &total_size, &buffers] {
    uint32_t size = fbl::round_up(size_distribution(prng), zx_system_get_page_size());
    total_size += size;

    auto collection = make_single_participant_collection_v2();
    v2::BufferCollectionConstraints constraints;
    constraints.usage().emplace();
    constraints.usage()->video() = v2::kVideoUsageHwDecoder;
    constexpr uint32_t kBufferCount = 1;
    constraints.min_buffer_count_for_camping() = 1;
    constraints.buffer_memory_constraints().emplace();
    auto& buffer_memory = constraints.buffer_memory_constraints().value();
    buffer_memory.min_size_bytes() = size;
    buffer_memory.max_size_bytes() = size;
    buffer_memory.physically_contiguous_required() = true;
    buffer_memory.secure_required() = true;
    buffer_memory.ram_domain_supported() = false;
    buffer_memory.cpu_domain_supported() = false;
    buffer_memory.inaccessible_domain_supported() = true;
    buffer_memory.heap_permitted() = {v2::HeapType::kAmlogicSecure};
    ZX_DEBUG_ASSERT(!constraints.image_format_constraints().has_value());

    v2::BufferCollectionSetConstraintsRequest set_constraints_request;
    set_constraints_request.constraints() = std::move(constraints);
    ASSERT_TRUE(collection->SetConstraints(std::move(set_constraints_request)).is_ok());

    // This is the first round-trip to/from sysmem.  A failure here can be due
    // to any step above failing async.
    auto allocate_result = collection->WaitForAllBuffersAllocated();

    ASSERT_TRUE(allocate_result.is_ok());
    auto& buffer_collection_info = allocate_result->buffer_collection_info().value();
    EXPECT_EQ(buffer_collection_info.buffers()->size(), kBufferCount);
    EXPECT_EQ(buffer_collection_info.settings()->buffer_settings()->size_bytes().value(), size);
    EXPECT_TRUE(
        buffer_collection_info.settings()->buffer_settings()->is_physically_contiguous().value());
    EXPECT_TRUE(buffer_collection_info.settings()->buffer_settings()->is_secure().value());
    EXPECT_EQ(buffer_collection_info.settings()->buffer_settings()->coherency_domain().value(),
              v2::CoherencyDomain::kInaccessible);
    EXPECT_EQ(buffer_collection_info.settings()->buffer_settings()->heap().value(),
              v2::HeapType::kAmlogicSecure);
    EXPECT_FALSE(buffer_collection_info.settings()->image_format_constraints().has_value());
    EXPECT_EQ(buffer_collection_info.buffers()->at(0).vmo_usable_start().value(), 0);

    zx::vmo buffer = std::move(buffer_collection_info.buffers()->at(0).vmo().value());
    buffer_collection_info.buffers()->at(0).vmo().reset();

    buffers.emplace(
        std::make_pair(key_distribution(prng), Vmo{.size = size, .vmo = std::move(buffer)}));
  };
  auto remove = [&get_random_buffer, &buffers, &total_size] {
    auto random_buffer = get_random_buffer();
    EXPECT_NE(random_buffer, buffers.end());
    total_size -= random_buffer->second.size;
    buffers.erase(random_buffer);
  };
  auto check_random_buffer_page = [&get_random_buffer] {
    auto random_buffer_iter = get_random_buffer();
    SecureVmoReadTester tester(zx::unowned_vmo(random_buffer_iter->second.vmo));
    ASSERT_DEATH(([&] { tester.AttemptReadFromSecure(); }));
    ASSERT_FALSE(tester.IsReadFromSecureAThing());
  };

  const uint32_t kIterations = 1000;
  for (uint32_t i = 0; i < kIterations; ++i) {
    if (i % 100 == 0) {
      printf("iteration: %u\n", i);
    }
    while (total_size > kTotalSizeThreshold) {
      remove();
    }
    uint32_t roll = op_distribution(prng);
    switch (roll) {
      // add
      case 0 ... 48:
        if (total_size < kTotalSizeThreshold) {
          add();
        }
        break;
      // fill
      case 49:
        while (total_size < kTotalSizeThreshold) {
          add();
        }
        break;
      // remove
      case 50 ... 98:
        if (total_size) {
          remove();
        }
        break;
      // clear
      case 99:
        while (total_size) {
          remove();
        }
        break;
      case 100 ... 104:
        if (total_size) {
          check_random_buffer_page();
        }
        break;
    }
  }

  for (uint32_t i = 0; i < 64; ++i) {
    auto collection = make_single_participant_collection_v2();

    v2::BufferCollectionConstraints constraints;
    constraints.usage().emplace();
    constraints.usage()->video() = v2::kVideoUsageHwDecoder;
    constexpr uint32_t kBufferCount = 4;
    constraints.min_buffer_count_for_camping() = kBufferCount;
    constraints.buffer_memory_constraints().emplace();
    constexpr uint32_t kBufferSizeBytes = 64 * 1024;
    auto& buffer_memory = constraints.buffer_memory_constraints().value();
    buffer_memory.min_size_bytes() = kBufferSizeBytes;
    buffer_memory.max_size_bytes() = 128 * 1024;
    buffer_memory.physically_contiguous_required() = true;
    buffer_memory.secure_required() = true;
    buffer_memory.ram_domain_supported() = false;
    buffer_memory.cpu_domain_supported() = false;
    buffer_memory.inaccessible_domain_supported() = true;
    buffer_memory.heap_permitted() = {v2::HeapType::kAmlogicSecure};
    ZX_DEBUG_ASSERT(!constraints.image_format_constraints().has_value());

    v2::BufferCollectionSetConstraintsRequest set_constraints_request;
    set_constraints_request.constraints() = std::move(constraints);
    ASSERT_TRUE(collection->SetConstraints(std::move(set_constraints_request)).is_ok());

    auto allocate_result = collection->WaitForAllBuffersAllocated();
    // This is the first round-trip to/from sysmem.  A failure here can be due
    // to any step above failing async.
    ASSERT_TRUE(allocate_result.is_ok());
    auto& buffer_collection_info = allocate_result->buffer_collection_info().value();
    EXPECT_EQ(buffer_collection_info.buffers()->size(), kBufferCount);
    EXPECT_EQ(buffer_collection_info.settings()->buffer_settings()->size_bytes().value(),
              kBufferSizeBytes);
    EXPECT_TRUE(
        buffer_collection_info.settings()->buffer_settings()->is_physically_contiguous().value());
    EXPECT_TRUE(buffer_collection_info.settings()->buffer_settings()->is_secure().value());
    EXPECT_EQ(buffer_collection_info.settings()->buffer_settings()->coherency_domain().value(),
              v2::CoherencyDomain::kInaccessible);
    EXPECT_EQ(buffer_collection_info.settings()->buffer_settings()->heap().value(),
              v2::HeapType::kAmlogicSecure);
    EXPECT_FALSE(buffer_collection_info.settings()->image_format_constraints().has_value());

    for (uint32_t i = 0; i < buffer_collection_info.buffers()->size(); ++i) {
      EXPECT_NE(buffer_collection_info.buffers()->at(i).vmo()->get(), ZX_HANDLE_INVALID);
      uint64_t size_bytes = 0;
      auto status =
          zx_vmo_get_size(buffer_collection_info.buffers()->at(i).vmo()->get(), &size_bytes);
      ASSERT_EQ(status, ZX_OK);
      EXPECT_EQ(size_bytes, kBufferSizeBytes);
    }

    zx::vmo the_vmo = std::move(buffer_collection_info.buffers()->at(0).vmo().value());
    buffer_collection_info.buffers()->at(0).vmo().reset();
    SecureVmoReadTester tester(std::move(the_vmo));
    ASSERT_DEATH(([&] { tester.AttemptReadFromSecure(); }));
    ASSERT_FALSE(tester.IsReadFromSecureAThing());
  }
}

TEST(Sysmem, HeapAmlogicSecureOnlySupportsInaccessibleV2) {
  if (!is_board_with_amlogic_secure()) {
    return;
  }

  v2::CoherencyDomain domains[] = {
      v2::CoherencyDomain::kCpu,
      v2::CoherencyDomain::kRam,
      v2::CoherencyDomain::kInaccessible,
  };

  for (const v2::CoherencyDomain domain : domains) {
    auto collection = make_single_participant_collection_v2();

    v2::BufferCollectionConstraints constraints;
    constraints.usage().emplace();
    constraints.usage()->video() = v2::kVideoUsageHwDecoder;
    constexpr uint32_t kBufferCount = 4;
    constraints.min_buffer_count_for_camping() = kBufferCount;
    constraints.buffer_memory_constraints().emplace();
    auto& buffer_memory = constraints.buffer_memory_constraints().value();
    constexpr uint32_t kBufferSizeBytes = 64 * 1024;
    buffer_memory.min_size_bytes() = kBufferSizeBytes;
    buffer_memory.max_size_bytes() = 128 * 1024;
    buffer_memory.physically_contiguous_required() = true;
    buffer_memory.secure_required() = true;
    buffer_memory.ram_domain_supported() = (domain == v2::CoherencyDomain::kRam);
    buffer_memory.cpu_domain_supported() = (domain == v2::CoherencyDomain::kCpu);
    buffer_memory.inaccessible_domain_supported() = (domain == v2::CoherencyDomain::kInaccessible);
    buffer_memory.heap_permitted() = {v2::HeapType::kAmlogicSecure};
    ZX_DEBUG_ASSERT(!constraints.image_format_constraints().has_value());

    v2::BufferCollectionSetConstraintsRequest set_constraints_request;
    set_constraints_request.constraints() = std::move(constraints);
    ASSERT_TRUE(collection->SetConstraints(std::move(set_constraints_request)).is_ok());

    auto allocate_result = collection->WaitForAllBuffersAllocated();

    if (domain == v2::CoherencyDomain::kInaccessible) {
      // This is the first round-trip to/from sysmem.  A failure here can be due
      // to any step above failing async.
      ASSERT_TRUE(allocate_result.is_ok());
      auto& buffer_collection_info = allocate_result->buffer_collection_info().value();

      EXPECT_EQ(buffer_collection_info.buffers()->size(), kBufferCount);
      EXPECT_EQ(buffer_collection_info.settings()->buffer_settings()->size_bytes().value(),
                kBufferSizeBytes);
      EXPECT_TRUE(
          buffer_collection_info.settings()->buffer_settings()->is_physically_contiguous().value());
      EXPECT_TRUE(buffer_collection_info.settings()->buffer_settings()->is_secure().value());
      EXPECT_EQ(buffer_collection_info.settings()->buffer_settings()->coherency_domain().value(),
                v2::CoherencyDomain::kInaccessible);
      EXPECT_EQ(buffer_collection_info.settings()->buffer_settings()->heap().value(),
                v2::HeapType::kAmlogicSecure);
      EXPECT_FALSE(buffer_collection_info.settings()->image_format_constraints().has_value());
    } else {
      ASSERT_FALSE(allocate_result.is_ok(),
                   "Sysmem should not allocate memory from secure heap with coherency domain %u",
                   static_cast<uint32_t>(domain));
    }
  }
}

TEST(Sysmem, HeapAmlogicSecureVdecV2) {
  if (!is_board_with_amlogic_secure_vdec()) {
    return;
  }

  for (uint32_t i = 0; i < 64; ++i) {
    auto collection = make_single_participant_collection_v2();

    v2::BufferCollectionConstraints constraints;
    constraints.usage().emplace();
    constraints.usage()->video() = fuchsia_sysmem::wire::kVideoUsageDecryptorOutput |
                                   fuchsia_sysmem::wire::kVideoUsageHwDecoder;
    constexpr uint32_t kBufferCount = 4;
    constraints.min_buffer_count_for_camping() = kBufferCount;
    constraints.buffer_memory_constraints().emplace();
    auto& buffer_memory = constraints.buffer_memory_constraints().value();
    constexpr uint32_t kBufferSizeBytes = 64 * 1024 - 1;
    buffer_memory.min_size_bytes() = kBufferSizeBytes;
    buffer_memory.max_size_bytes() = 128 * 1024;
    buffer_memory.physically_contiguous_required() = true;
    buffer_memory.secure_required() = true;
    buffer_memory.ram_domain_supported() = false;
    buffer_memory.cpu_domain_supported() = false;
    buffer_memory.inaccessible_domain_supported() = true;
    buffer_memory.heap_permitted() = {v2::HeapType::kAmlogicSecureVdec};
    ZX_DEBUG_ASSERT(!constraints.image_format_constraints().has_value());

    v2::BufferCollectionSetConstraintsRequest set_constraints_request;
    set_constraints_request.constraints() = std::move(constraints);
    ASSERT_TRUE(collection->SetConstraints(std::move(set_constraints_request)).is_ok());

    auto allocate_result = collection->WaitForAllBuffersAllocated();
    // This is the first round-trip to/from sysmem.  A failure here can be due
    // to any step above failing async.
    ASSERT_TRUE(allocate_result.is_ok());
    auto& buffer_collection_info = allocate_result->buffer_collection_info().value();

    EXPECT_EQ(buffer_collection_info.buffers()->size(), kBufferCount);
    EXPECT_EQ(buffer_collection_info.settings()->buffer_settings()->size_bytes().value(),
              kBufferSizeBytes);
    EXPECT_EQ(
        buffer_collection_info.settings()->buffer_settings()->is_physically_contiguous().value(),
        true);
    EXPECT_EQ(buffer_collection_info.settings()->buffer_settings()->is_secure().value(), true);
    EXPECT_EQ(buffer_collection_info.settings()->buffer_settings()->coherency_domain().value(),
              v2::CoherencyDomain::kInaccessible);
    EXPECT_EQ(buffer_collection_info.settings()->buffer_settings()->heap().value(),
              v2::HeapType::kAmlogicSecureVdec);
    EXPECT_FALSE(buffer_collection_info.settings()->image_format_constraints().has_value());

    auto expected_size = fbl::round_up(kBufferSizeBytes, zx_system_get_page_size());
    for (uint32_t i = 0; i < buffer_collection_info.buffers()->size(); ++i) {
      EXPECT_NE(buffer_collection_info.buffers()->at(i).vmo()->get(), ZX_HANDLE_INVALID);
      uint64_t size_bytes = 0;
      auto status =
          zx_vmo_get_size(buffer_collection_info.buffers()->at(i).vmo()->get(), &size_bytes);
      ASSERT_EQ(status, ZX_OK);
      EXPECT_EQ(size_bytes, expected_size);
    }

    zx::vmo the_vmo = std::move(buffer_collection_info.buffers()->at(0).vmo().value());
    buffer_collection_info.buffers()->at(0).vmo().reset();
    SecureVmoReadTester tester(std::move(the_vmo));
    ASSERT_DEATH(([&] { tester.AttemptReadFromSecure(); }));
    ASSERT_FALSE(tester.IsReadFromSecureAThing());
  }
}

TEST(Sysmem, CpuUsageAndInaccessibleDomainSupportedSucceedsV2) {
  auto collection = make_single_participant_collection_v2();

  constexpr uint32_t kBufferCount = 3;
  constexpr uint32_t kBufferSize = 64 * 1024;
  v2::BufferCollectionConstraints constraints;
  constraints.usage().emplace();
  constraints.usage()->cpu() = v2::kCpuUsageReadOften | v2::kCpuUsageWriteOften;
  constraints.min_buffer_count_for_camping() = kBufferCount;
  constraints.buffer_memory_constraints().emplace();
  auto& buffer_memory = constraints.buffer_memory_constraints().value();
  buffer_memory.min_size_bytes() = kBufferSize;
  buffer_memory.max_size_bytes() = 128 * 1024;
  buffer_memory.physically_contiguous_required() = false;
  buffer_memory.secure_required() = false;
  buffer_memory.ram_domain_supported() = false;
  buffer_memory.cpu_domain_supported() = true;
  buffer_memory.inaccessible_domain_supported() = true;
  ZX_DEBUG_ASSERT(!buffer_memory.heap_permitted().has_value());
  ZX_DEBUG_ASSERT(!constraints.image_format_constraints().has_value());

  v2::BufferCollectionSetConstraintsRequest set_constraints_request;
  set_constraints_request.constraints() = std::move(constraints);
  ASSERT_TRUE(collection->SetConstraints(std::move(set_constraints_request)).is_ok());

  auto allocate_result = collection->WaitForAllBuffersAllocated();
  // This is the first round-trip to/from sysmem.  A failure here can be due
  // to any step above failing async.
  ASSERT_TRUE(allocate_result.is_ok());
  auto& buffer_collection_info = allocate_result->buffer_collection_info().value();

  ASSERT_EQ(buffer_collection_info.buffers()->size(), kBufferCount);
  ASSERT_EQ(buffer_collection_info.settings()->buffer_settings()->size_bytes().value(),
            kBufferSize);
  ASSERT_FALSE(
      buffer_collection_info.settings()->buffer_settings()->is_physically_contiguous().value());
  ASSERT_FALSE(buffer_collection_info.settings()->buffer_settings()->is_secure().value());
  ASSERT_EQ(buffer_collection_info.settings()->buffer_settings()->coherency_domain().value(),
            v2::CoherencyDomain::kCpu);
  ASSERT_FALSE(buffer_collection_info.settings()->image_format_constraints().has_value());

  for (uint32_t i = 0; i < buffer_collection_info.buffers()->size(); ++i) {
    ASSERT_NE(buffer_collection_info.buffers()->at(i).vmo()->get(), ZX_HANDLE_INVALID);
    uint64_t size_bytes = 0;
    auto status =
        zx_vmo_get_size(buffer_collection_info.buffers()->at(i).vmo()->get(), &size_bytes);
    ASSERT_EQ(status, ZX_OK);
    ASSERT_EQ(size_bytes, kBufferSize);
  }
}

TEST(Sysmem, AllocatedBufferZeroInRamV2) {
  constexpr uint32_t kBufferCount = 1;
  // Since we're reading from buffer start to buffer end, let's not allocate too large a buffer,
  // since perhaps that'd hide problems if the cache flush is missing in sysmem.
  constexpr uint32_t kBufferSize = 64 * 1024;
  constexpr uint32_t kIterationCount = 200;

  auto zero_buffer = std::make_unique<uint8_t[]>(kBufferSize);
  ASSERT_TRUE(zero_buffer);
  auto tmp_buffer = std::make_unique<uint8_t[]>(kBufferSize);
  ASSERT_TRUE(tmp_buffer);
  for (uint32_t iter = 0; iter < kIterationCount; ++iter) {
    auto collection = make_single_participant_collection_v2();

    v2::BufferCollectionConstraints constraints;
    constraints.usage().emplace();
    constraints.usage()->cpu() = v2::kCpuUsageReadOften | v2::kCpuUsageWriteOften;
    constraints.min_buffer_count_for_camping() = kBufferCount;
    constraints.buffer_memory_constraints().emplace();
    auto& buffer_memory = constraints.buffer_memory_constraints().value();
    buffer_memory.min_size_bytes() = kBufferSize;
    buffer_memory.max_size_bytes() = kBufferSize;
    buffer_memory.physically_contiguous_required() = false;
    buffer_memory.secure_required() = false;
    buffer_memory.ram_domain_supported() = false;
    buffer_memory.cpu_domain_supported() = true;
    buffer_memory.inaccessible_domain_supported() = false;
    ZX_DEBUG_ASSERT(!buffer_memory.heap_permitted().has_value());
    ZX_DEBUG_ASSERT(!constraints.image_format_constraints().has_value());

    v2::BufferCollectionSetConstraintsRequest set_constraints_request;
    set_constraints_request.constraints() = std::move(constraints);
    ASSERT_TRUE(collection->SetConstraints(std::move(set_constraints_request)).is_ok());

    auto allocate_result = collection->WaitForAllBuffersAllocated();
    // This is the first round-trip to/from sysmem.  A failure here can be due
    // to any step above failing async.
    ASSERT_TRUE(allocate_result.is_ok());

    // We intentionally don't check a bunch of stuff here.  We assume that sysmem allocated
    // kBufferCount (1) buffer of kBufferSize (64 KiB).  That way we're comparing ASAP after buffer
    // allocation, in case that helps catch any failure to actually zero in RAM.  Ideally we'd read
    // using a DMA in this test instead of using CPU reads, but that wouldn't be a portable test.
    auto& buffer_collection_info = allocate_result->buffer_collection_info().value();
    zx::vmo vmo = std::move(buffer_collection_info.buffers()->at(0).vmo().value());
    buffer_collection_info.buffers()->at(0).vmo().reset();

    // Before we read from the VMO, we need to invalidate cache for the VMO.  We do this via a
    // syscall since it seems like mapping would have a greater chance of doing a fence.
    // Unfortunately none of these steps are guaranteed not to hide a problem with flushing or fence
    // in sysmem...
    auto status =
        vmo.op_range(ZX_VMO_OP_CACHE_INVALIDATE, /*offset=*/0, kBufferSize, /*buffer=*/nullptr,
                     /*buffer_size=*/0);
    ASSERT_EQ(status, ZX_OK);

    // Read using a syscall instead of mapping, just in case mapping would do a bigger fence.
    status = vmo.read(tmp_buffer.get(), 0, kBufferSize);
    ASSERT_EQ(status, ZX_OK);

    // Any non-zero bytes could be a problem with sysmem's zeroing, or cache flushing, or fencing of
    // the flush (depending on whether a given architecture is willing to cancel a cache line flush
    // on later cache line invalidate, which would seem at least somewhat questionable, and may not
    // be a thing).  This not catching a problem doesn't mean there are no problems, so that's why
    // we loop kIterationCount times to see if we can detect a problem.
    EXPECT_EQ(0, memcmp(zero_buffer.get(), tmp_buffer.get(), kBufferSize));

    // These should be noticed by sysmem before we've allocated enough space in the loop to cause
    // any trouble allocating:
    // ~vmo
    // ~collection_client
  }
}

// Test that most image format constraints don't need to be specified.
TEST(Sysmem, DefaultAttributesV2) {
  auto collection = make_single_participant_collection_v2();

  v2::BufferCollectionConstraints constraints;
  constraints.usage().emplace();
  constraints.usage()->cpu() = v2::kCpuUsageReadOften | v2::kCpuUsageWriteOften;
  constraints.min_buffer_count_for_camping() = 1;
  ZX_DEBUG_ASSERT(!constraints.buffer_memory_constraints().has_value());
  constraints.image_format_constraints().emplace(1);
  auto& image_constraints = constraints.image_format_constraints()->at(0);
  image_constraints.pixel_format() = fuchsia_images2::PixelFormat::kNv12;
  image_constraints.color_spaces().emplace(1);
  image_constraints.color_spaces()->at(0) = fuchsia_images2::ColorSpace::kRec709;
  image_constraints.required_max_size().emplace();
  image_constraints.required_max_size()->width() = 512;
  image_constraints.required_max_size()->height() = 1024;

  v2::BufferCollectionSetConstraintsRequest set_constraints_request;
  set_constraints_request.constraints() = std::move(constraints);
  ASSERT_TRUE(collection->SetConstraints(std::move(set_constraints_request)).is_ok());

  auto allocate_result = collection->WaitForAllBuffersAllocated();
  // This is the first round-trip to/from sysmem.  A failure here can be due
  // to any step above failing async.
  ASSERT_TRUE(allocate_result.is_ok());
  auto& buffer_collection_info = allocate_result->buffer_collection_info().value();

  size_t vmo_size;
  auto status = zx_vmo_get_size(buffer_collection_info.buffers()->at(0).vmo()->get(), &vmo_size);
  ASSERT_EQ(status, ZX_OK);

  // Image must be at least 512x1024 NV12, due to the required max sizes
  // above.
  EXPECT_LE(512 * 1024 * 3 / 2, vmo_size);
}

// Check that the sending FIDL code validates how many image format constraints there are.
TEST(Sysmem, TooManyFormatsV2) {
  auto allocator = connect_to_sysmem_driver_v2();
  ASSERT_TRUE(allocator.is_ok());

  zx::result token_endpoints = fidl::CreateEndpoints<v2::BufferCollectionToken>();
  ASSERT_TRUE(token_endpoints.is_ok());
  auto [token_client_end, token_server_end] = std::move(*token_endpoints);

  v2::AllocatorAllocateSharedCollectionRequest allocate_shared_request;
  allocate_shared_request.token_request() = std::move(token_server_end);
  ASSERT_TRUE(allocator->AllocateSharedCollection(std::move(allocate_shared_request)).is_ok());

  zx::result collection_endpoints = fidl::CreateEndpoints<v2::BufferCollection>();
  ASSERT_TRUE(collection_endpoints.is_ok());
  auto [collection_client_end, collection_server_end] = std::move(*collection_endpoints);

  EXPECT_NE(token_client_end.channel().get(), ZX_HANDLE_INVALID);

  v2::AllocatorBindSharedCollectionRequest bind_shared_request;
  bind_shared_request.token() = std::move(token_client_end);
  bind_shared_request.buffer_collection_request() = std::move(collection_server_end);
  ASSERT_TRUE(allocator->BindSharedCollection(std::move(bind_shared_request)).is_ok());

  fidl::SyncClient collection{std::move(collection_client_end)};

  v2::BufferCollectionConstraints constraints;
  constraints.usage().emplace();
  constraints.usage()->cpu() = v2::kCpuUsageReadOften | v2::kCpuUsageWriteOften;
  constraints.min_buffer_count_for_camping() = 1;
  constraints.buffer_memory_constraints().emplace();
  auto& buffer_memory = constraints.buffer_memory_constraints().value();
  buffer_memory.min_size_bytes() = 1;
  constraints.image_format_constraints().emplace(
      v2::kMaxCountBufferCollectionConstraintsImageFormatConstraints + 1);
  for (uint32_t i = 0; i < v2::kMaxCountBufferCollectionConstraintsImageFormatConstraints + 1;
       i++) {
    auto& image_constraints = constraints.image_format_constraints()->at(i);
    image_constraints.pixel_format() = fuchsia_images2::PixelFormat::kNv12;
    image_constraints.color_spaces().emplace(1);
    image_constraints.color_spaces()->at(0) = fuchsia_images2::ColorSpace::kRec709;
    image_constraints.required_max_size().emplace();
    image_constraints.required_max_size()->width() = 512;
    image_constraints.required_max_size()->height() = 1024;
  }

  v2::BufferCollectionSetConstraintsRequest set_constraints_request;
  set_constraints_request.constraints() = std::move(constraints);
  // The FIDL sending code will immediately notice there are too many image_foramt_constraints(),
  // before the server ever sees the message.
  ASSERT_FALSE(collection->SetConstraints(std::move(set_constraints_request)).is_ok());

  // Verify that the server didn't crash, and that the server never saw the above SetConstraints().
  // The FIDL sending code didn't close the channel during the above SetConstraints().
  ASSERT_TRUE(collection->Sync().is_ok());
}

// Check that the server checks for min_buffer_count too large.
TEST(Sysmem, TooManyBuffersV2) {
  auto allocator = connect_to_sysmem_driver_v2();

  zx::result collection_endpoints = fidl::CreateEndpoints<v2::BufferCollection>();
  ASSERT_TRUE(collection_endpoints.is_ok());
  auto [collection_client_end, collection_server_end] = std::move(*collection_endpoints);
  fidl::SyncClient collection{std::move(collection_client_end)};

  v2::AllocatorAllocateNonSharedCollectionRequest allocate_non_shared_request;
  allocate_non_shared_request.collection_request() = std::move(collection_server_end);
  ASSERT_TRUE(
      allocator->AllocateNonSharedCollection(std::move(allocate_non_shared_request)).is_ok());

  SetDefaultCollectionNameV2(collection);

  v2::BufferCollectionConstraints constraints;
  constraints.buffer_memory_constraints().emplace();
  constraints.buffer_memory_constraints()->min_size_bytes() = zx_system_get_page_size();
  constraints.min_buffer_count() =
      1024 * 1024 * 1024 / constraints.buffer_memory_constraints()->min_size_bytes().value();
  constraints.buffer_memory_constraints()->cpu_domain_supported() = true;
  constraints.usage()->cpu() = v2::kCpuUsageRead;

  v2::BufferCollectionSetConstraintsRequest set_constraints_request;
  set_constraints_request.constraints() = std::move(constraints);
  ASSERT_TRUE(collection->SetConstraints(std::move(set_constraints_request)).is_ok());

  auto allocation_result = collection->WaitForAllBuffersAllocated();
  EXPECT_FALSE(allocation_result.is_ok());

  verify_connectivity_v2(*allocator);
}

bool BasicAllocationSucceedsV2(
    fit::function<void(v2::BufferCollectionConstraints& to_modify)> modify_constraints) {
  auto collection = make_single_participant_collection_v2();

  v2::BufferCollectionConstraints constraints;
  constraints.usage().emplace();
  constraints.usage()->cpu() = v2::kCpuUsageReadOften | v2::kCpuUsageWriteOften;
  constraints.min_buffer_count_for_camping() = 3;
  constraints.buffer_memory_constraints().emplace();
  auto& buffer_memory = constraints.buffer_memory_constraints().value();
  // This min_size_bytes is intentionally too small to hold the min_coded_width and
  // min_coded_height in NV12
  // format.
  buffer_memory.min_size_bytes() = 64 * 1024;
  buffer_memory.max_size_bytes() = 128 * 1024;
  buffer_memory.physically_contiguous_required() = false;
  buffer_memory.secure_required() = false;
  buffer_memory.ram_domain_supported() = false;
  buffer_memory.cpu_domain_supported() = true;
  buffer_memory.inaccessible_domain_supported() = false;
  ZX_DEBUG_ASSERT(!buffer_memory.heap_permitted().has_value());
  constraints.image_format_constraints().emplace(1);
  auto& image_constraints = constraints.image_format_constraints()->at(0);
  image_constraints.pixel_format() = fuchsia_images2::PixelFormat::kNv12;
  image_constraints.color_spaces().emplace(1);
  image_constraints.color_spaces()->at(0) = fuchsia_images2::ColorSpace::kRec709;
  // The min dimensions intentionally imply a min size that's larger than
  // buffer_memory_constraints.min_size_bytes.
  image_constraints.min_size() = {256, 256};
  image_constraints.max_size() = {std::numeric_limits<uint32_t>::max(),
                                  std::numeric_limits<uint32_t>::max()};
  image_constraints.min_bytes_per_row() = 256;
  image_constraints.max_bytes_per_row() = std::numeric_limits<uint32_t>::max();
  image_constraints.max_surface_width_times_surface_height() = std::numeric_limits<uint32_t>::max();
  image_constraints.size_alignment() = {2, 2};
  image_constraints.bytes_per_row_divisor() = 2;
  image_constraints.start_offset_divisor() = 2;
  image_constraints.display_rect_alignment() = {1, 1};

  modify_constraints(constraints);

  v2::BufferCollectionSetConstraintsRequest set_constraints_request;
  set_constraints_request.constraints() = std::move(constraints);
  EXPECT_TRUE(collection->SetConstraints(std::move(set_constraints_request)).is_ok());

  auto allocate_result = collection->WaitForAllBuffersAllocated();

  // This is the first round-trip to/from sysmem.  A failure here can be due
  // to any step above failing async.
  if (!allocate_result.is_ok()) {
    if (allocate_result.error_value().is_framework_error()) {
      printf("WaitForBuffersAllocated failed - framework status: %d\n",
             allocate_result.error_value().framework_error().status());
    } else {
      printf("WaitForBuffersAllocated failed - domain status: %d\n",
             allocate_result.error_value().domain_error());
    }
    return false;
  }

  // Check if the contents in allocated VMOs are already filled with zero.
  // If the allocated VMO is readable, then we would expect it could be cleared by sysmem;
  // otherwise we just skip this check.
  auto& buffer_collection_info = allocate_result->buffer_collection_info().value();

  zx::vmo allocated_vmo = std::move(buffer_collection_info.buffers()->at(0).vmo().value());
  buffer_collection_info.buffers()->at(0).vmo().reset();
  size_t vmo_size;
  auto status = allocated_vmo.get_size(&vmo_size);
  if (status != ZX_OK) {
    printf("ERROR: Cannot get size of allocated_vmo: %d\n", status);
    return false;
  }

  size_t size_bytes = std::min(
      vmo_size, static_cast<size_t>(
                    buffer_collection_info.settings()->buffer_settings()->size_bytes().value()));
  std::vector<uint8_t> bytes(size_bytes, 0xff);

  status = allocated_vmo.read(bytes.data(), 0u, size_bytes);
  if (status == ZX_ERR_NOT_SUPPORTED) {
    // If the allocated VMO is not readable, then we just skip value check,
    // since we do not expect it being cleared by write syscalls.
    printf("INFO: allocated_vmo doesn't support zx_vmo_read, skip value check\n");
    return true;
  }

  // Check if all the contents we read from the VMO are filled with zero.
  return *std::max_element(bytes.begin(), bytes.end()) == 0u;
}

TEST(Sysmem, BasicAllocationSucceedsV2) {
  EXPECT_TRUE(BasicAllocationSucceedsV2([](v2::BufferCollectionConstraints& to_modify_nop) {}));
}

TEST(Sysmem, ZeroMinSizeBytesFailsV2) {
  EXPECT_FALSE(BasicAllocationSucceedsV2([](v2::BufferCollectionConstraints& to_modify) {
    // Disable image_format_constraints so that the client is not specifying any min size via
    // implied by image_format_constraints.
    to_modify.image_format_constraints()->resize(0);
    // Also set 0 min_size_bytes, so that implied minimum overall size is 0.
    to_modify.buffer_memory_constraints()->min_size_bytes() = 0;
  }));
}

TEST(Sysmem, ZeroMaxBufferCount_FailsInV2) {
  // With sysmem2 this will be expected to fail.  With sysmem(1), this succeeds because 0 is
  // interpreted as replace with default.
  EXPECT_FALSE(BasicAllocationSucceedsV2(
      [](v2::BufferCollectionConstraints& to_modify) { to_modify.max_buffer_count() = 0; }));
}

TEST(Sysmem, ZeroRequiredMinCodedWidth_FailsInV2) {
  // With sysmem2 this will be expected to fail.  With sysmem(1), this succeeds because 0 is
  // interpreted as replace with default.
  EXPECT_FALSE(BasicAllocationSucceedsV2([](v2::BufferCollectionConstraints& to_modify) {
    auto& ifc = to_modify.image_format_constraints()->at(0);
    if (!ifc.required_min_size().has_value()) {
      ifc.required_min_size().emplace();
      // not testing height 0, so set height to 1
      ifc.required_min_size()->height() = 1;
    }
    ifc.required_min_size()->width() = 0;
  }));
}

TEST(Sysmem, ZeroRequiredMinCodedHeight_FailsInV2) {
  // With sysmem2 this will be expected to fail.  With sysmem(1), this succeeds because 0 is
  // interpreted as replace with default.
  EXPECT_FALSE(BasicAllocationSucceedsV2([](v2::BufferCollectionConstraints& to_modify) {
    auto& ifc = to_modify.image_format_constraints()->at(0);
    if (!ifc.required_min_size().has_value()) {
      ifc.required_min_size().emplace();
      // not testing width 0, so set width to 1
      ifc.required_min_size()->width() = 1;
    }
    ifc.required_min_size()->height() = 0;
  }));
}

TEST(Sysmem, DuplicateConstraintsFailsV2) {
  EXPECT_FALSE(BasicAllocationSucceedsV2([](v2::BufferCollectionConstraints& to_modify) {
    to_modify.image_format_constraints()->resize(2);
    to_modify.image_format_constraints()->at(1) = to_modify.image_format_constraints()->at(0);
  }));
}

TEST(Sysmem, AttachToken_BeforeAllocate_SuccessV2) {
  EXPECT_TRUE(AttachTokenSucceedsV2(
      true, false, [](v2::BufferCollectionConstraints& to_modify) {},
      [](v2::BufferCollectionConstraints& to_modify) {},
      [](v2::BufferCollectionConstraints& to_modify) {},
      [](v2::BufferCollectionInfo& to_verify) {}));
  IF_FAILURES_RETURN();
}

TEST(Sysmem, AttachToken_AfterAllocate_SuccessV2) {
  EXPECT_TRUE(AttachTokenSucceedsV2(
      false, false, [](v2::BufferCollectionConstraints& to_modify) {},
      [](v2::BufferCollectionConstraints& to_modify) {},
      [](v2::BufferCollectionConstraints& to_modify) {},
      [](v2::BufferCollectionInfo& to_verify) {}));
}

TEST(Sysmem, AttachToken_BeforeAllocate_AttachedFailedEarly_FailureV2) {
  // Despite the attached token failing early, this still verifies that the non-attached tokens
  // are still ok and the LogicalBufferCollection is still ok.
  EXPECT_FALSE(AttachTokenSucceedsV2(
      true, true, [](v2::BufferCollectionConstraints& to_modify) {},
      [](v2::BufferCollectionConstraints& to_modify) {},
      [](v2::BufferCollectionConstraints& to_modify) {},
      [](v2::BufferCollectionInfo& to_verify) {}));
}

TEST(Sysmem, AttachToken_BeforeAllocate_Failure_BufferSizesV2) {
  EXPECT_FALSE(AttachTokenSucceedsV2(
      true, false, [](v2::BufferCollectionConstraints& to_modify) {},
      [](v2::BufferCollectionConstraints& to_modify) {},
      [](v2::BufferCollectionConstraints& to_modify) {
        to_modify.buffer_memory_constraints()->max_size_bytes() = (512 * 512) * 3 / 2 - 1;
      },
      [](v2::BufferCollectionInfo& to_verify) {}));
}

TEST(Sysmem, AttachToken_AfterAllocate_Failure_BufferSizesV2) {
  EXPECT_FALSE(AttachTokenSucceedsV2(
      false, false, [](v2::BufferCollectionConstraints& to_modify) {},
      [](v2::BufferCollectionConstraints& to_modify) {},
      [](v2::BufferCollectionConstraints& to_modify) {
        to_modify.buffer_memory_constraints()->max_size_bytes() = (512 * 512) * 3 / 2 - 1;
      },
      [](v2::BufferCollectionInfo& to_verify) {}));
}

TEST(Sysmem, AttachToken_BeforeAllocate_Success_BufferCountsV2) {
  const uint32_t kAttachTokenBufferCount = 2;
  uint32_t buffers_needed = 0;
  EXPECT_TRUE(AttachTokenSucceedsV2(
      true, false,
      [&buffers_needed](v2::BufferCollectionConstraints& to_modify) {
        // 3
        buffers_needed += to_modify.min_buffer_count_for_camping().value();
      },
      [&buffers_needed](v2::BufferCollectionConstraints& to_modify) {
        // 3
        buffers_needed += to_modify.min_buffer_count_for_camping().value();
        // 8
        to_modify.min_buffer_count() = buffers_needed + kAttachTokenBufferCount;
      },
      [&](v2::BufferCollectionConstraints& to_modify) {
        // 2
        to_modify.min_buffer_count_for_camping() = kAttachTokenBufferCount;
      },
      [](v2::BufferCollectionInfo& to_verify) {},
      8  // max(8, 3 + 3 + 2)
      ));
}

TEST(Sysmem, AttachToken_AfterAllocate_Success_BufferCountsV2) {
  const uint32_t kAttachTokenBufferCount = 2;
  uint32_t buffers_needed = 0;
  EXPECT_TRUE(AttachTokenSucceedsV2(
      false, false,
      [&buffers_needed](v2::BufferCollectionConstraints& to_modify) {
        // 3
        buffers_needed += to_modify.min_buffer_count_for_camping().value();
      },
      [&buffers_needed](v2::BufferCollectionConstraints& to_modify) {
        // 3
        buffers_needed += to_modify.min_buffer_count_for_camping().value();
        // 8
        to_modify.min_buffer_count() = buffers_needed + kAttachTokenBufferCount;
      },
      [&](v2::BufferCollectionConstraints& to_modify) {
        // 2
        to_modify.min_buffer_count_for_camping() = kAttachTokenBufferCount;
      },
      [](v2::BufferCollectionInfo& to_verify) {},
      8  // max(8, 3 + 3)
      ));
}

TEST(Sysmem, AttachToken_BeforeAllocate_Failure_BufferCountsV2) {
  EXPECT_FALSE(AttachTokenSucceedsV2(
      true, false, [](v2::BufferCollectionConstraints& to_modify) {},
      [](v2::BufferCollectionConstraints& to_modify) {},
      [](v2::BufferCollectionConstraints& to_modify) {
        to_modify.min_buffer_count_for_camping() = 1;
      },
      [](v2::BufferCollectionInfo& to_verify) {},
      // Only 6 get allocated, despite AttachToken() before allocation, because we intentionally
      // want AttachToken() before vs. after initial allocation to behave as close to the same as
      // possible.
      6));
}

TEST(Sysmem, AttachToken_AfterAllocate_Failure_BufferCountsV2) {
  EXPECT_FALSE(AttachTokenSucceedsV2(
      false, false, [](v2::BufferCollectionConstraints& to_modify) {},
      [](v2::BufferCollectionConstraints& to_modify) {},
      [](v2::BufferCollectionConstraints& to_modify) {
        to_modify.min_buffer_count_for_camping() = 1;
      },
      [](v2::BufferCollectionInfo& to_verify) {},
      // Only 6 get allocated at first, then AttachToken() sequence started after initial allocation
      // fails (it would have failed even if it had started before initial allocation though).
      6));
}

TEST(Sysmem, AttachToken_SelectsSameDomainAsInitialAllocationV2) {
  // The first part is mostly to verify that we have a way of influencing an initial allocation
  // to pick RAM coherency domain, for the benefit of the second AttachTokenSucceeds() call below.
  EXPECT_TRUE(AttachTokenSucceedsV2(
      false, false,
      [](v2::BufferCollectionConstraints& to_modify) {
        to_modify.buffer_memory_constraints()->cpu_domain_supported() = true;
        to_modify.buffer_memory_constraints()->ram_domain_supported() = true;
        to_modify.buffer_memory_constraints()->inaccessible_domain_supported() = false;
        // This will influence the initial allocation to pick RAM.
        to_modify.usage()->display() = fuchsia_sysmem::wire::kDisplayUsageLayer;
        EXPECT_TRUE(to_modify.usage()->cpu().value() != 0);
      },
      [](v2::BufferCollectionConstraints& to_modify) {
        to_modify.buffer_memory_constraints()->cpu_domain_supported() = true;
        to_modify.buffer_memory_constraints()->ram_domain_supported() = true;
        to_modify.buffer_memory_constraints()->inaccessible_domain_supported() = false;
        EXPECT_TRUE(to_modify.usage()->cpu().value() != 0);
      },
      [](v2::BufferCollectionConstraints& to_modify) {
        to_modify.buffer_memory_constraints()->cpu_domain_supported() = true;
        to_modify.buffer_memory_constraints()->ram_domain_supported() = true;
        to_modify.buffer_memory_constraints()->inaccessible_domain_supported() = false;
        EXPECT_TRUE(to_modify.usage()->cpu().value() != 0);
      },
      [](v2::BufferCollectionInfo& to_verify) {
        EXPECT_EQ(v2::CoherencyDomain::kRam,
                  to_verify.settings()->buffer_settings()->coherency_domain().value());
      },
      6));
  // Now verify that if the initial allocation is CPU coherency domain, an attached token that would
  // normally prefer RAM domain can succeed but will get CPU because the initial allocation already
  // picked CPU.
  EXPECT_TRUE(AttachTokenSucceedsV2(
      false, false,
      [](v2::BufferCollectionConstraints& to_modify) {
        to_modify.buffer_memory_constraints()->cpu_domain_supported() = true;
        to_modify.buffer_memory_constraints()->ram_domain_supported() = true;
        to_modify.buffer_memory_constraints()->inaccessible_domain_supported() = false;
        EXPECT_TRUE(to_modify.usage()->cpu().value() != 0);
      },
      [](v2::BufferCollectionConstraints& to_modify) {
        to_modify.buffer_memory_constraints()->cpu_domain_supported() = true;
        to_modify.buffer_memory_constraints()->ram_domain_supported() = true;
        to_modify.buffer_memory_constraints()->inaccessible_domain_supported() = false;
        EXPECT_TRUE(to_modify.usage()->cpu().value() != 0);
      },
      [](v2::BufferCollectionConstraints& to_modify) {
        to_modify.buffer_memory_constraints()->cpu_domain_supported() = true;
        to_modify.buffer_memory_constraints()->ram_domain_supported() = true;
        to_modify.buffer_memory_constraints()->inaccessible_domain_supported() = false;
        // This would normally influence to pick RAM coherency domain, but because the existing
        // BufferCollectionInfo says CPU, the attached participant will get CPU instead of its
        // normally-preferred RAM.
        to_modify.usage()->display() = v2::kDisplayUsageLayer;
        EXPECT_TRUE(to_modify.usage()->cpu().value() != 0);
      },
      [](v2::BufferCollectionInfo& to_verify) {
        EXPECT_EQ(v2::CoherencyDomain::kCpu,
                  to_verify.settings()->buffer_settings()->coherency_domain().value());
      },
      6));
}

TEST(Sysmem, SetDispensableV2) {
  enum class Variant { kDispensableFailureBeforeAllocation, kDispensableFailureAfterAllocation };
  constexpr Variant variants[] = {Variant::kDispensableFailureBeforeAllocation,
                                  Variant::kDispensableFailureAfterAllocation};
  for (Variant variant : variants) {
    auto allocator = connect_to_sysmem_driver_v2();

    auto token_endpoints_1 = fidl::CreateEndpoints<v2::BufferCollectionToken>();
    ASSERT_TRUE(token_endpoints_1.is_ok());
    auto [token_client_1, token_server_1] = std::move(*token_endpoints_1);
    fidl::SyncClient token_1{std::move(token_client_1)};

    // Client 1 creates a token and new LogicalBufferCollection using
    // AllocateSharedCollection().
    v2::AllocatorAllocateSharedCollectionRequest allocate_shared_request;
    allocate_shared_request.token_request() = std::move(token_server_1);
    ASSERT_TRUE(allocator->AllocateSharedCollection(std::move(allocate_shared_request)).is_ok());

    auto token_endpoints_2 = fidl::CreateEndpoints<v2::BufferCollectionToken>();
    ASSERT_TRUE(token_endpoints_2.is_ok());
    auto [token_client_2, token_server_2] = std::move(*token_endpoints_2);
    fidl::SyncClient token_2{std::move(token_client_2)};

    // Client 1 duplicates its token and gives the duplicate to client 2 (this
    // test is single proc, so both clients are coming from this client
    // process - normally the two clients would be in separate processes with
    // token_client_2 transferred to another participant).
    v2::BufferCollectionTokenDuplicateRequest duplicate_request;
    duplicate_request.rights_attenuation_mask() = ZX_RIGHT_SAME_RIGHTS;
    duplicate_request.token_request() = std::move(token_server_2);
    ASSERT_TRUE(token_1->Duplicate(std::move(duplicate_request)).is_ok());

    // Client 1 calls SetDispensable() on token 2.  Client 2's constraints will be part of the
    // initial allocation, but post-allocation, client 2 failure won't cause failure of the
    // LogicalBufferCollection.
    ASSERT_TRUE(token_2->SetDispensable().is_ok());

    auto collection_endpoints_1 = fidl::CreateEndpoints<v2::BufferCollection>();
    ASSERT_TRUE(collection_endpoints_1.is_ok());
    auto [collection_client_1, collection_server_1] = std::move(*collection_endpoints_1);
    fidl::SyncClient collection_1{std::move(collection_client_1)};

    ASSERT_NE(token_1.client_end().channel().get(), ZX_HANDLE_INVALID);

    v2::AllocatorBindSharedCollectionRequest bind_shared_request;
    bind_shared_request.token() = token_1.TakeClientEnd();
    bind_shared_request.buffer_collection_request() = std::move(collection_server_1);
    ASSERT_TRUE(allocator->BindSharedCollection(std::move(bind_shared_request)).is_ok());

    v2::BufferCollectionConstraints constraints_1;
    constraints_1.usage().emplace();
    constraints_1.usage()->cpu() = v2::kCpuUsageReadOften | v2::kCpuUsageWriteOften;
    constraints_1.min_buffer_count_for_camping() = 2;
    constraints_1.buffer_memory_constraints().emplace();
    auto& buffer_memory = constraints_1.buffer_memory_constraints().value();
    buffer_memory.min_size_bytes() = 64 * 1024;
    buffer_memory.max_size_bytes() = 64 * 1024;
    buffer_memory.physically_contiguous_required() = false;
    buffer_memory.secure_required() = false;
    buffer_memory.ram_domain_supported() = false;
    buffer_memory.cpu_domain_supported() = true;
    buffer_memory.inaccessible_domain_supported() = false;
    ZX_DEBUG_ASSERT(!buffer_memory.heap_permitted().has_value());

    // constraints_2 is just a copy of constraints_1 - since both participants
    // specify min_buffer_count_for_camping 2, the total number of allocated
    // buffers will be 4.  There are no handles in the constraints struct so a
    // struct copy instead of clone is fine here.
    v2::BufferCollectionConstraints constraints_2(constraints_1);
    ASSERT_EQ(constraints_2.min_buffer_count_for_camping().value(), 2);

    // Client 2 connects to sysmem separately.
    auto allocator_2 = connect_to_sysmem_driver_v2();
    ASSERT_TRUE(allocator_2.is_ok());

    auto collection_endpoints_2 = fidl::CreateEndpoints<v2::BufferCollection>();
    ASSERT_TRUE(collection_endpoints_2.is_ok());
    auto [collection_client_2, collection_server_2] = std::move(*collection_endpoints_2);
    fidl::SyncClient collection_2{std::move(collection_client_2)};

    // Just because we can, perform this sync as late as possible, just before
    // the BindSharedCollection() via allocator2_client_2.  Without this Sync(),
    // the BindSharedCollection() might arrive at the server before the
    // Duplicate() that delivered the server end of token_client_2 to sysmem,
    // which would cause sysmem to not recognize the token.
    ASSERT_TRUE(collection_1->Sync().is_ok());

    ASSERT_NE(token_2.client_end().channel().get(), ZX_HANDLE_INVALID);

    v2::AllocatorBindSharedCollectionRequest bind_shared_request2;
    bind_shared_request2.token() = token_2.TakeClientEnd();
    bind_shared_request2.buffer_collection_request() = std::move(collection_server_2);
    ASSERT_TRUE(allocator_2->BindSharedCollection(std::move(bind_shared_request2)).is_ok());

    v2::BufferCollectionSetConstraintsRequest set_constraints_request;
    set_constraints_request.constraints() = std::move(constraints_1);
    ASSERT_TRUE(collection_1->SetConstraints(std::move(set_constraints_request)).is_ok());

    if (variant == Variant::kDispensableFailureBeforeAllocation) {
      // Client 2 will now abruptly close its channel.  Since client 2 hasn't provided constraints
      // yet, the LogicalBufferCollection will fail.
      collection_2 = {};
    } else {
      // Client 2 SetConstraints().

      v2::BufferCollectionSetConstraintsRequest set_constraints_request2;
      set_constraints_request2.constraints() = std::move(constraints_2);
      ASSERT_TRUE(collection_2->SetConstraints(std::move(set_constraints_request2)).is_ok());
    }

    //
    // kDispensableFailureAfterAllocation - The LogicalBufferCollection won't fail.
    auto allocate_result_1 = collection_1->WaitForAllBuffersAllocated();

    if (variant == Variant::kDispensableFailureBeforeAllocation) {
      // The LogicalBufferCollection will be failed, because client 2 failed before providing
      // constraints.
      ASSERT_FALSE(allocate_result_1.is_ok());
      ASSERT_TRUE(allocate_result_1.error_value().is_framework_error());
      ASSERT_EQ(allocate_result_1.error_value().framework_error().status(), ZX_ERR_PEER_CLOSED);
      // next variant, if any
      continue;
    }
    ZX_DEBUG_ASSERT(variant == Variant::kDispensableFailureAfterAllocation);

    // The LogicalBufferCollection will not be failed, because client 2 didn't fail before
    // allocation.
    ASSERT_TRUE(allocate_result_1.is_ok());
    // This being 4 instead of 2 proves that client 2's constraints were used.
    ASSERT_EQ(allocate_result_1->buffer_collection_info()->buffers()->size(), 4);

    // Now that we know allocation is done, client 2 will abruptly close its channel, which the
    // server treats as client 2 failure.  Since client 2 has already provided constraints, this
    // won't fail the LogicalBufferCollection.
    collection_2 = {};

    // Give the LogicalBufferCollection time to fail if it were going to fail, which it isn't.
    nanosleep_duration(zx::msec(250));

    // Verify LogicalBufferCollection still ok.
    ASSERT_TRUE(collection_1->Sync().is_ok());

    // next variant, if any
  }
}

TEST(Sysmem, IsAlternateFor_FalseV2) {
  auto root_token = create_initial_token_v2();
  auto token_a = create_token_under_token_v2(root_token);
  auto token_b = create_token_under_token_v2(root_token);
  auto maybe_response = token_b->GetNodeRef();
  ASSERT_TRUE(maybe_response.is_ok());
  auto token_b_node_ref = std::move(maybe_response->node_ref().value());
  v2::NodeIsAlternateForRequest is_alternate_request;
  is_alternate_request.node_ref() = std::move(token_b_node_ref);
  auto result = token_a->IsAlternateFor(std::move(is_alternate_request));
  ASSERT_TRUE(result.is_ok());
  EXPECT_FALSE(result->is_alternate().value());
}

TEST(Sysmem, IsAlternateFor_TrueV2) {
  auto root_token = create_initial_token_v2();
  auto group = create_group_under_token_v2(root_token);
  auto token_a = create_token_under_group_v2(group);
  auto token_b = create_token_under_group_v2(group);
  auto maybe_response = token_b->GetNodeRef();
  ASSERT_TRUE(maybe_response.is_ok());
  auto token_b_node_ref = std::move(maybe_response->node_ref().value());
  v2::NodeIsAlternateForRequest is_alternate_request;
  is_alternate_request.node_ref() = std::move(token_b_node_ref);
  auto result = token_a->IsAlternateFor(std::move(is_alternate_request));
  ASSERT_TRUE(result.is_ok());
  EXPECT_TRUE(result->is_alternate().value());
}

TEST(Sysmem, IsAlternateFor_MiniStressV2) {
  std::random_device random_device;
  std::uint_fast64_t seed{random_device()};
  std::mt19937_64 prng{seed};
  std::uniform_int_distribution<uint32_t> uint32_distribution(0,
                                                              std::numeric_limits<uint32_t>::max());

  // We use shared_ptr<> in this test to make it easier to share code between
  // cases below.
  std::vector<std::shared_ptr<TokenV2>> tokens;
  std::vector<std::shared_ptr<GroupV2>> groups;

  auto create_extra_group = [&](std::shared_ptr<TokenV2> token) {
    auto group = std::make_shared<GroupV2>(create_group_under_token_v2(*token));
    groups.emplace_back(group);

    auto child_0 = std::make_shared<TokenV2>(create_token_under_group_v2(*group));
    tokens.emplace_back(child_0);

    auto child_1 = std::make_shared<TokenV2>(create_token_under_group_v2(*group));
    tokens.emplace_back(token);
    token = child_1;

    return token;
  };

  auto create_child_chain = [&](std::shared_ptr<TokenV2> token, uint32_t additional_tokens) {
    for (uint32_t i = 0; i < additional_tokens; ++i) {
      if (i == additional_tokens / 2 && uint32_distribution(prng) % 2 == 0) {
        token = create_extra_group(token);
      } else {
        auto child_token = std::make_shared<TokenV2>(create_token_under_token_v2(*token));
        check_token_alive_v2(*child_token);
        tokens.emplace_back(token);
        token = child_token;
      }
    }
    return token;
  };

  constexpr uint32_t kIterations = 50;
  for (uint32_t i = 0; i < kIterations; ++i) {
    if ((i + 1) % 100 == 0) {
      printf("iteration cardinal: %u\n", i + 1);
    }

    tokens.clear();
    groups.clear();
    auto root_token = std::make_shared<TokenV2>(create_initial_token_v2());
    tokens.emplace_back(root_token);
    auto branch_token = create_child_chain(root_token, uint32_distribution(prng) % 5);
    std::shared_ptr<TokenV2> child_a;
    std::shared_ptr<TokenV2> child_b;
    bool is_alternate_for = !!(uint32_distribution(prng) % 2);
    if (is_alternate_for) {
      auto group = std::make_shared<GroupV2>(create_group_under_token_v2(*branch_token));
      groups.emplace_back(group);
      check_group_alive_v2(*group);
      // Both can remain direct children of group, or can become indirect children of group.
      child_a = std::make_shared<TokenV2>(create_token_under_group_v2(*group));
      child_b = std::make_shared<TokenV2>(create_token_under_group_v2(*group));
    } else {
      // Both can remain branch_token, either can be parent of the other, or both can be children
      // (direct or indirect) of branch_token.
      child_a = branch_token;
      child_b = branch_token;
    }
    child_a = create_child_chain(child_a, uint32_distribution(prng) % 5);
    child_b = create_child_chain(child_b, uint32_distribution(prng) % 5);
    auto maybe_node_ref_b = (*child_b)->GetNodeRef();
    ASSERT_TRUE(maybe_node_ref_b.is_ok());
    auto node_ref_b = std::move(maybe_node_ref_b->node_ref().value());
    v2::NodeIsAlternateForRequest is_alternate_request;
    is_alternate_request.node_ref() = std::move(node_ref_b);
    auto result = (*child_a)->IsAlternateFor(std::move(is_alternate_request));
    ASSERT_TRUE(result.is_ok());
    EXPECT_EQ(is_alternate_for, result->is_alternate().value());
  }
}

TEST(Sysmem, GroupPrefersFirstChildV2) {
  const uint32_t kFirstChildSize = zx_system_get_page_size() * 16;
  const uint32_t kSecondChildSize = zx_system_get_page_size() * 32;

  auto root_token = create_initial_token_v2();
  auto group = create_group_under_token_v2(root_token);
  auto child_0_token = create_token_under_group_v2(group);
  auto child_1_token = create_token_under_group_v2(group);

  auto root = convert_token_to_collection_v2(std::move(root_token));
  auto child_0 = convert_token_to_collection_v2(std::move(child_0_token));
  auto child_1 = convert_token_to_collection_v2(std::move(child_1_token));
  v2::BufferCollectionSetConstraintsRequest set_constraints_request;
  set_constraints_request.constraints().emplace();
  auto set_result = root->SetConstraints(std::move(set_constraints_request));
  ASSERT_TRUE(set_result.is_ok());
  set_picky_constraints_v2(child_0, kFirstChildSize);
  set_picky_constraints_v2(child_1, kSecondChildSize);

  auto all_children_present_result = group->AllChildrenPresent();
  ASSERT_TRUE(all_children_present_result.is_ok());

  auto wait_result1 = root->WaitForAllBuffersAllocated();
  ASSERT_TRUE(wait_result1.is_ok());
  auto info = std::move(wait_result1->buffer_collection_info().value());
  ASSERT_EQ(info.settings()->buffer_settings()->size_bytes().value(), kFirstChildSize);

  auto wait_result2 = child_0->WaitForAllBuffersAllocated();
  ASSERT_TRUE(wait_result2.is_ok());
  info = std::move(wait_result2->buffer_collection_info().value());
  ASSERT_EQ(info.settings()->buffer_settings()->size_bytes().value(), kFirstChildSize);

  auto wait_result3 = child_1->WaitForAllBuffersAllocated();
  // Most clients calling this way won't get the epitaph; this test doesn't either.
  ASSERT_FALSE(wait_result3.is_ok());
  ASSERT_TRUE(wait_result3.error_value().is_framework_error() &&
                  wait_result3.error_value().framework_error().status() == ZX_ERR_PEER_CLOSED ||
              wait_result3.error_value().is_domain_error() &&
                  wait_result3.error_value().domain_error() == ZX_ERR_NOT_SUPPORTED);
}

TEST(Sysmem, GroupPriorityV2) {
  constexpr uint32_t kIterations = 20;
  for (uint32_t i = 0; i < kIterations; ++i) {
    if ((i + 1) % 100 == 0) {
      printf("iteration cardinal: %u\n", i + 1);
    }

    // We determine which group is lowest priority according to sysmem by noticing which group
    // selects its ordinal 1 child instead of its ordinal 0 child.  We force one group to pick the
    // ordinal 1 child by setting max_buffer_count to 1 buffer less than can be satisfied with all
    // ordinal 0 children which each specify min_buffer_count_for_camping
    // 1.  In contrast, the ordinal 1 children specify min_buffer_count_for_camping 0, so selecting
    // a single ordinal 1 child is enough to succeed aggregation.

    uint32_t constraints_count = 0;

    std::random_device random_device;
    std::uint_fast64_t seed{random_device()};
    std::mt19937_64 prng{seed};
    std::uniform_int_distribution<uint32_t> uint32_distribution(
        0, std::numeric_limits<uint32_t>::max());

    constexpr uint32_t kGroupCount = 4;
    constexpr uint32_t kNonGroupCount = 12;
    constexpr uint32_t kPickCount = 2;

    using Item = std::variant<std::monostate, SharedTokenV2, SharedCollectionV2, SharedGroupV2>;
    struct Node;
    using SharedNode = std::shared_ptr<Node>;
    struct Node {
      bool is_group() { return item.index() == 3; }
      Node* parent = nullptr;
      std::vector<SharedNode> children;
      // For a group, true means this group's direct child tokens all have is_camper true, and false
      // means the second (ordinal 1) child has is_camper false.
      //
      // For a token, true means this token will specify min_buffer_count_for_camping 1, and false
      // means this token will specify min_buffer_count_for_camping 0.
      bool is_camper = true;
      Item item;
      std::optional<uint32_t> which_child;
      uint32_t picked_group_dfs_preorder_ordinal;
    };
    std::vector<SharedNode> all_nodes;
    std::vector<SharedNode> group_nodes;
    std::vector<SharedNode> un_picked_group_nodes;
    std::vector<SharedNode> picked_group_nodes;
    std::vector<SharedNode> non_group_nodes;
    std::vector<SharedNode> pre_groups_non_group_nodes;

    SharedNode root_node;

    auto create_node = [&](Node* parent, Item item) {
      auto node = std::make_shared<Node>();
      node->parent = parent;
      bool is_parent_a_group = false;
      if (parent) {
        parent->children.emplace_back(node);
        is_parent_a_group = parent->is_group();
      }
      node->item = std::move(item);
      all_nodes.emplace_back(node);
      if (node->is_group()) {
        group_nodes.emplace_back(node);
        un_picked_group_nodes.emplace_back(node);
      } else {
        non_group_nodes.emplace_back(node);
        // this condition works for tallying constraints count only because we don't have nested
        // groups in this test
        if (!is_parent_a_group || parent->children.size() == 1) {
          ++constraints_count;
        }
      }
      if (group_nodes.empty() && !node->is_group()) {
        pre_groups_non_group_nodes.emplace_back(node);
      }
      return node;
    };

    // This has a bias toward having more nodes directly under nodes that existed for longer, but
    // that's fine.
    auto add_random_nodes = [&]() {
      for (uint32_t i = 0; i < kNonGroupCount; ++i) {
        auto parent_node = all_nodes[uint32_distribution(prng) % all_nodes.size()];
        auto node =
            create_node(parent_node.get(), std::make_shared<TokenV2>(create_token_under_token_v2(
                                               *std::get<SharedTokenV2>(parent_node->item))));
      }
      for (uint32_t i = 0; i < kGroupCount; ++i) {
        auto parent_node = pre_groups_non_group_nodes[uint32_distribution(prng) %
                                                      pre_groups_non_group_nodes.size()];
        auto group =
            create_node(parent_node.get(), std::make_shared<GroupV2>(create_group_under_token_v2(
                                               *std::get<SharedTokenV2>(parent_node->item))));
        // a group must have at least one child so go ahead and add that child when we add the group
        create_node(group.get(), std::make_shared<TokenV2>(create_token_under_group_v2(
                                     *std::get<SharedGroupV2>(group->item))));
      }
    };

    auto pick_groups = [&]() {
      for (uint32_t i = 0; i < kPickCount; ++i) {
        uint32_t which = uint32_distribution(prng) % un_picked_group_nodes.size();
        auto group_node = un_picked_group_nodes[which];
        group_node->is_camper = false;
        un_picked_group_nodes.erase(un_picked_group_nodes.begin() + which);
        picked_group_nodes.emplace_back(group_node);
        auto group = std::get<SharedGroupV2>(group_node->item);
        auto token = create_node(group_node.get(),
                                 std::make_shared<TokenV2>(create_token_under_group_v2(*group)));
        token->is_camper = false;
      }
    };

    auto set_picked_group_dfs_preorder_ordinals = [&] {
      struct StackLevel {
        SharedNode node;
        uint32_t next_child = 0;
      };
      uint32_t ordinal = 0;
      std::vector<StackLevel> stack;
      stack.emplace_back(StackLevel{.node = root_node, .next_child = 0});
      while (!stack.empty()) {
        auto& cur = stack.back();
        if (cur.next_child == 0) {
          // visit
          if (cur.node->is_group() && !cur.node->is_camper) {
            cur.node->picked_group_dfs_preorder_ordinal = ordinal;
            ++ordinal;
          }
        }
        if (cur.next_child == cur.node->children.size()) {
          stack.pop_back();
          continue;
        }
        auto child = cur.node->children[cur.next_child];
        ++cur.next_child;
        stack.push_back(StackLevel{.node = child, .next_child = 0});
      }
    };

    auto finalize_nodes = [&] {
      for (auto& node : all_nodes) {
        if (node->is_group()) {
          auto group = std::get<SharedGroupV2>(node->item);
          EXPECT_TRUE((*group)->AllChildrenPresent().is_ok());
        } else {
          auto token = std::get<SharedTokenV2>(node->item);
          auto collection =
              std::make_shared<CollectionV2>(convert_token_to_collection_v2(std::move(*token)));
          node->item.emplace<SharedCollectionV2>(collection);
          if (node.get() == root_node.get()) {
            set_min_camping_constraints_v2(*collection, node->is_camper ? 1 : 0,
                                           constraints_count - 1);
          } else {
            set_min_camping_constraints_v2(*collection, node->is_camper ? 1 : 0);
          }
        }
      }
    };

    auto check_nodes = [&] {
      for (auto& node : all_nodes) {
        if (node->is_group()) {
          continue;
        }
        auto collection = std::get<SharedCollectionV2>(node->item);
        auto wait_result = (*collection)->WaitForAllBuffersAllocated();
        if (!wait_result.is_ok()) {
          auto* group = node->parent;
          ASSERT_FALSE(group->is_camper);
          // Only the picked group enumerated last among the picked groups (in DFS pre-order) gives
          // up on camping on a buffer by failing its camping child 0 and using it's non-camping
          // child 1 instead.  The picked groups with higher priority (more important) instaed keep
          // their camping child 0 and fail their non-camping child 1.
          ASSERT_EQ((group->picked_group_dfs_preorder_ordinal < kPickCount - 1), !node->is_camper);
        }
      }
    };

    root_node = create_node(nullptr, std::make_shared<TokenV2>(create_initial_token_v2()));
    add_random_nodes();
    pick_groups();
    set_picked_group_dfs_preorder_ordinals();
    finalize_nodes();
    check_nodes();
  }
}

TEST(Sysmem, Group_MiniStressV2) {
  // In addition to some degree of stress, this tests whether sysmem can find the group child
  // selections that work, given various arrangements of tokens, groups, and constraints
  // compatibility / incompatibility.
  constexpr uint32_t kIterations = 50;
  for (uint32_t i = 0; i < kIterations; ++i) {
    if ((i + 1) % 100 == 0) {
      printf("iteration cardinal: %u\n", i + 1);
    }
    const uint32_t kCompatibleSize = zx_system_get_page_size();
    const uint32_t kIncompatibleSize = 2 * zx_system_get_page_size();
    // We can't go too high here, since we allow groups, and all the groups could end up as sibling
    // groups whose child selections don't hide any of the other groups, leading to a high number of
    // group child selection combinations.
    //
    // Child 0 of a group doesn't count as a "random" child since every group must have at least one
    // child.
    const uint32_t kRandomChildrenCount = 6;

    using Item = std::variant<SharedTokenV2, SharedCollectionV2, SharedGroupV2>;

    std::random_device random_device;
    std::uint_fast64_t seed{random_device()};
    std::mt19937_64 prng{seed};
    std::uniform_int_distribution<uint32_t> uint32_distribution(
        0, std::numeric_limits<uint32_t>::max());

    struct Node;
    using SharedNode = std::shared_ptr<Node>;
    struct Node {
      bool is_group() { return item.index() == 2; }
      Node* parent;
      std::vector<SharedNode> children;
      bool is_compatible;
      Item item;
      std::optional<uint32_t> which_child;
    };
    std::vector<SharedNode> all_nodes;

    auto create_node = [&](Node* parent, Item item) {
      auto node = std::make_shared<Node>();
      node->parent = parent;
      if (parent) {
        parent->children.emplace_back(node);
      }
      node->is_compatible = false;
      node->item = std::move(item);
      all_nodes.emplace_back(node);
      return node;
    };

    // This has a bias toward having more nodes directly under nodes that existed for longer, but
    // that's fine.
    auto add_random_nodes = [&]() {
      for (uint32_t i = 0; i < kRandomChildrenCount; ++i) {
        auto parent_node = all_nodes[uint32_distribution(prng) % all_nodes.size()];
        SharedNode node;
        if (parent_node->is_group()) {
          node =
              create_node(parent_node.get(), std::make_shared<TokenV2>(create_token_under_group_v2(
                                                 *std::get<SharedGroupV2>(parent_node->item))));
        } else if (uint32_distribution(prng) % 2 == 0) {
          node =
              create_node(parent_node.get(), std::make_shared<TokenV2>(create_token_under_token_v2(
                                                 *std::get<SharedTokenV2>(parent_node->item))));
        } else {
          node =
              create_node(parent_node.get(), std::make_shared<GroupV2>(create_group_under_token_v2(
                                                 *std::get<SharedTokenV2>(parent_node->item))));
          // a group must have at least one child so go ahead and add that child when we add the
          // group
          create_node(node.get(), std::make_shared<TokenV2>(create_token_under_group_v2(
                                      *std::get<SharedGroupV2>(node->item))));
        }
      }
    };

    auto select_group_children = [&] {
      for (auto& node : all_nodes) {
        if (!node->is_group()) {
          continue;
        }
        node->which_child = {uint32_distribution(prng) % node->children.size()};
      }
    };

    auto is_visible = [&](SharedNode node) {
      for (Node* iter = node.get(); iter; iter = iter->parent) {
        if (!iter->parent) {
          return true;
        }
        Node* parent = iter->parent;
        if (!parent->is_group()) {
          continue;
        }
        EXPECT_TRUE(parent->which_child.has_value());
        if (parent->children[*parent->which_child].get() != iter) {
          return false;
        }
      }
      ZX_PANIC("impossible");
    };

    auto find_visible_nodes_and_mark_compatible = [&] {
      for (auto& node : all_nodes) {
        if (!is_visible(node)) {
          continue;
        }
        if (node->is_group()) {
          continue;
        }
        node->is_compatible = true;
      }
    };

    auto finalize_nodes = [&] {
      std::atomic<uint32_t> ready_threads = 0;
      std::atomic<bool> threads_go = false;
      std::vector<std::thread> threads;
      threads.reserve(all_nodes.size());
      for (auto& node : all_nodes) {
        threads.emplace_back(std::thread([&] {
          ++ready_threads;
          while (!threads_go) {
            std::this_thread::yield();
          }
          if (node->is_group()) {
            auto group = std::get<SharedGroupV2>(node->item);
            EXPECT_TRUE((*group)->AllChildrenPresent().is_ok());
          } else {
            auto token = std::get<SharedTokenV2>(node->item);
            auto collection =
                std::make_shared<CollectionV2>(convert_token_to_collection_v2(std::move(*token)));
            node->item.emplace<SharedCollectionV2>(collection);
            uint32_t size = node->is_compatible ? kCompatibleSize : kIncompatibleSize;
            set_picky_constraints_v2(*collection, size);
          }
        }));
      }
      while (ready_threads != threads.size()) {
        std::this_thread::yield();
      }
      threads_go = true;
      for (auto& thread : threads) {
        thread.join();
      }
    };

    auto check_nodes = [&] {
      for (auto& node : all_nodes) {
        if (node->is_group()) {
          continue;
        }
        auto collection = std::get<SharedCollectionV2>(node->item);
        auto wait_result = (*collection)->WaitForAllBuffersAllocated();
        if (!node->is_compatible) {
          EXPECT_FALSE(wait_result.is_ok());
          if (wait_result.error_value().is_domain_error()) {
            EXPECT_STATUS(wait_result.error_value().domain_error(), ZX_ERR_NOT_SUPPORTED);
          } else {
            EXPECT_STATUS(wait_result.error_value().framework_error().status(), ZX_ERR_PEER_CLOSED);
          }
          continue;
        }
        EXPECT_TRUE(wait_result.is_ok());
        EXPECT_EQ(wait_result->buffer_collection_info()
                      ->settings()
                      ->buffer_settings()
                      ->size_bytes()
                      .value(),
                  kCompatibleSize);
      }
    };

    auto root_node = create_node(nullptr, std::make_shared<TokenV2>(create_initial_token_v2()));
    add_random_nodes();
    select_group_children();
    find_visible_nodes_and_mark_compatible();
    finalize_nodes();
    check_nodes();
  }
}

TEST(Sysmem, SkipUnreachableChildSelectionsV2) {
  const uint32_t kCompatibleSize = zx_system_get_page_size();
  const uint32_t kIncompatibleSize = 2 * zx_system_get_page_size();
  std::vector<std::shared_ptr<TokenV2>> incompatible_tokens;
  std::vector<std::shared_ptr<TokenV2>> compatible_tokens;
  std::vector<std::shared_ptr<GroupV2>> groups;
  std::vector<std::shared_ptr<CollectionV2>> collections;
  auto root_token = std::make_shared<TokenV2>(create_initial_token_v2());
  compatible_tokens.emplace_back(root_token);
  auto cur_token = root_token;
  // Essentially counting to 2^63 would take too long and cause the test to time out.  The fact
  // that the test doesn't time out shows that sysmem isn't trying all the unreachable child
  // selections.
  //
  // We use 63 instead of 64 to avoid hitting kMaxGroupChildCombinations.
  //
  // While this sysmem behavior can help cut down on the amount of unnecessary work as sysmem is
  // enumerating through all potentially useful combinations of group child selections, this
  // behavior doesn't prevent constructing a bunch of sibling groups (unlike this test which uses
  // child groups) each with two options (for example) where only the right children can all
  // succeed (for example).  In that case (and others with many reachable child selection
  // combinations) sysmem would be forced to give up after trying several group child combinations
  // instead of ever finding the highest priority combination that can succeed aggregation.
  for (uint32_t i = 0; i < 63; ++i) {
    auto group = std::make_shared<GroupV2>(create_group_under_token_v2(*cur_token));
    groups.emplace_back(group);
    // We create the next_token first, because we want to prefer the deeper sub-tree.
    auto next_token = std::make_shared<TokenV2>(create_token_under_group_v2(*group));
    incompatible_tokens.emplace_back(next_token);
    auto shorter_child = std::make_shared<TokenV2>(create_token_under_group_v2(*group));
    compatible_tokens.emplace_back(shorter_child);
    cur_token = next_token;
  }
  for (auto& token : compatible_tokens) {
    auto collection =
        std::make_shared<CollectionV2>(convert_token_to_collection_v2(std::move(*token)));
    collections.emplace_back(collection);
    set_picky_constraints_v2(*collection, kCompatibleSize);
  }
  for (auto& token : incompatible_tokens) {
    auto collection =
        std::make_shared<CollectionV2>(convert_token_to_collection_v2(std::move(*token)));
    collections.emplace_back(collection);
    set_picky_constraints_v2(*collection, kIncompatibleSize);
  }
  for (auto& group : groups) {
    auto result = (*group)->AllChildrenPresent();
    ASSERT_TRUE(result.is_ok());
  }
  // Only two collections will succeed - the root and the right child of the group direclty under
  // the root.
  std::vector<std::shared_ptr<CollectionV2>> success_collections;
  auto root_collection = collections[0];
  auto right_child_under_root_collection = collections[1];
  success_collections.emplace_back(root_collection);
  success_collections.emplace_back(right_child_under_root_collection);

  // Remove the success collections so we can conveniently iterate over the failed collections.
  //
  // This is not efficient, but it's a test, and it's not super slow.
  collections.erase(collections.begin());
  collections.erase(collections.begin());

  for (auto& collection : success_collections) {
    auto wait_result = (*collection)->WaitForAllBuffersAllocated();
    ASSERT_TRUE(wait_result.is_ok());
    auto info = std::move(wait_result->buffer_collection_info().value());
    ASSERT_EQ(info.settings()->buffer_settings()->size_bytes().value(), kCompatibleSize);
  }

  for (auto& collection : collections) {
    auto wait_result = (*collection)->WaitForAllBuffersAllocated();
    ASSERT_FALSE(wait_result.is_ok());
    ASSERT_TRUE(wait_result.error_value().is_framework_error() &&
                    wait_result.error_value().framework_error().status() == ZX_ERR_PEER_CLOSED ||
                wait_result.error_value().is_domain_error() &&
                    wait_result.error_value().domain_error() == ZX_ERR_NOT_SUPPORTED);
  }
}

TEST(Sysmem, GroupChildSelectionCombinationCountLimitV2) {
  const uint32_t kCompatibleSize = zx_system_get_page_size();
  const uint32_t kIncompatibleSize = 2 * zx_system_get_page_size();
  std::vector<std::shared_ptr<TokenV2>> incompatible_tokens;
  std::vector<std::shared_ptr<TokenV2>> compatible_tokens;
  std::vector<std::shared_ptr<GroupV2>> groups;
  std::vector<std::shared_ptr<CollectionV2>> collections;
  auto root_token = std::make_shared<TokenV2>(create_initial_token_v2());
  incompatible_tokens.emplace_back(root_token);
  // Essentially counting to 2^64 would take too long and cause the test to time out.  The fact
  // that the test doesn't time out (and instead the logical allocation fails) shows that sysmem
  // isn't trying all the group child selections, by design.
  for (uint32_t i = 0; i < 64; ++i) {
    auto group = std::make_shared<GroupV2>(create_group_under_token_v2(*root_token));
    groups.emplace_back(group);
    auto child_token_0 = std::make_shared<TokenV2>(create_token_under_group_v2(*group));
    compatible_tokens.emplace_back(child_token_0);
    auto child_token_1 = std::make_shared<TokenV2>(create_token_under_group_v2(*group));
    compatible_tokens.emplace_back(child_token_1);
  }
  for (auto& token : compatible_tokens) {
    auto collection =
        std::make_shared<CollectionV2>(convert_token_to_collection_v2(std::move(*token)));
    collections.emplace_back(collection);
    set_picky_constraints_v2(*collection, kCompatibleSize);
  }
  for (auto& token : incompatible_tokens) {
    auto collection =
        std::make_shared<CollectionV2>(convert_token_to_collection_v2(std::move(*token)));
    collections.emplace_back(collection);
    set_picky_constraints_v2(*collection, kIncompatibleSize);
  }
  for (auto& group : groups) {
    auto result = (*group)->AllChildrenPresent();
    ASSERT_TRUE(result.is_ok());
  }
  // Because the root has constraints that are incompatible with any group child token, the logical
  // allocation will fail (eventually).  The fact that it fails in a duration that avoids the test
  // timing out is passing the test.
  for (auto& collection : collections) {
    // If we get stuck here, it means the test will time out, which is effectively a failure.
    auto wait_result = (*collection)->WaitForAllBuffersAllocated();
    ASSERT_FALSE(wait_result.is_ok());
    ASSERT_TRUE(wait_result.error_value().is_framework_error() &&
                    wait_result.error_value().framework_error().status() == ZX_ERR_PEER_CLOSED ||
                wait_result.error_value().is_domain_error() &&
                    wait_result.error_value().domain_error() == ZX_ERR_OUT_OF_RANGE);
  }
}

TEST(Sysmem, GroupCreateChildrenSyncV2) {
  for (uint32_t is_oddball_writable = 0; is_oddball_writable < 2; ++is_oddball_writable) {
    const uint32_t kCompatibleSize = zx_system_get_page_size();
    const uint32_t kIncompatibleSize = 2 * zx_system_get_page_size();
    auto root_token = create_initial_token_v2();
    auto group = create_group_under_token_v2(root_token);
    check_group_alive_v2(group);
    constexpr uint32_t kTokenCount = 16;
    std::vector<zx_rights_t> rights_attenuation_masks;
    rights_attenuation_masks.resize(kTokenCount, ZX_RIGHT_SAME_RIGHTS);
    constexpr uint32_t kOddballIndex = kTokenCount / 2;
    if (!is_oddball_writable) {
      rights_attenuation_masks[kOddballIndex] = (0xFFFFFFFF & ~ZX_RIGHT_WRITE);
      EXPECT_EQ((rights_attenuation_masks[kOddballIndex] & ZX_RIGHT_WRITE), 0);
    }
    v2::BufferCollectionTokenGroupCreateChildrenSyncRequest children_sync_request;
    children_sync_request.rights_attenuation_masks() = std::move(rights_attenuation_masks);
    auto result = group->CreateChildrenSync(std::move(children_sync_request));
    ASSERT_TRUE(result.is_ok());
    std::vector<fidl::SyncClient<v2::BufferCollectionToken>> tokens;
    for (auto& token_client : result->tokens().value()) {
      tokens.emplace_back(fidl::SyncClient(std::move(token_client)));
    }
    tokens.emplace_back(std::move(root_token));
    auto is_root = [&](uint32_t index) { return index == tokens.size() - 1; };
    auto is_oddball = [&](uint32_t index) { return index == kOddballIndex; };
    auto is_compatible = [&](uint32_t index) { return is_oddball(index) || is_root(index); };
    std::vector<fidl::SyncClient<v2::BufferCollection>> collections;
    collections.reserve(tokens.size());
    for (auto& token : tokens) {
      collections.emplace_back(convert_token_to_collection_v2(std::move(token)));
    }
    uint32_t index = 0;
    for (auto& collection : collections) {
      uint32_t size = is_compatible(index) ? kCompatibleSize : kIncompatibleSize;
      set_picky_constraints_v2(collection, size);
      ++index;
    }
    auto group_done_result = group->AllChildrenPresent();
    ASSERT_TRUE(group_done_result.is_ok());
    index = 0;
    for (auto& collection : collections) {
      auto inc_index = fit::defer([&] { ++index; });
      auto wait_result = collection->WaitForAllBuffersAllocated();
      if (!is_compatible(index)) {
        ASSERT_FALSE(wait_result.is_ok());
        ASSERT_TRUE(wait_result.error_value().is_framework_error() &&
                        wait_result.error_value().framework_error().status() ==
                            ZX_ERR_PEER_CLOSED ||
                    wait_result.error_value().is_domain_error() &&
                        wait_result.error_value().domain_error() == ZX_ERR_NOT_SUPPORTED);
        continue;
      }
      ASSERT_TRUE(wait_result.is_ok());
      auto info = std::move(wait_result->buffer_collection_info().value());
      // root and oddball both said min_buffer_count_for_camping 1
      ASSERT_EQ(info.buffers()->size(), 2);
      ASSERT_EQ(info.settings()->buffer_settings()->size_bytes().value(), kCompatibleSize);
      zx::unowned_vmo vmo(info.buffers()->at(0).vmo().value());
      zx_info_handle_basic_t basic_info{};
      EXPECT_OK(
          vmo->get_info(ZX_INFO_HANDLE_BASIC, &basic_info, sizeof(basic_info), nullptr, nullptr));
      uint8_t data = 42;
      zx_status_t write_status = vmo->write(&data, 0, sizeof(data));
      if (is_root(index)) {
        ASSERT_OK(write_status);
      } else {
        EXPECT_TRUE(is_oddball(index));
        if (is_oddball_writable) {
          ASSERT_OK(write_status);
        } else {
          ASSERT_EQ(write_status, ZX_ERR_ACCESS_DENIED);
        }
      }
    }
  }
}

TEST(Sysmem, SetVerboseLoggingV2) {
  // Verbose logging shouldn't have any observable effect via the sysmem protocols, so this test
  // mainly just checks that having verbose logging enabled for a failed allocation doesn't crash
  // sysmem.  In addition, the log output during this test can be manually checked to make sure it's
  // significantly more verbose as intended.

  auto root_token = create_initial_token_v2();
  auto set_verbose_result = root_token->SetVerboseLogging();
  ASSERT_TRUE(set_verbose_result.is_ok());
  auto child_token = create_token_under_token_v2(root_token);
  auto child2_token = create_token_under_token_v2(child_token);
  check_token_alive_v2(child_token);
  check_token_alive_v2(child2_token);
  const uint32_t kCompatibleSize = zx_system_get_page_size();
  const uint32_t kIncompatibleSize = 2 * zx_system_get_page_size();
  auto root = convert_token_to_collection_v2(std::move(root_token));
  auto child = convert_token_to_collection_v2(std::move(child_token));
  auto child2 = convert_token_to_collection_v2(std::move(child2_token));
  set_picky_constraints_v2(root, kCompatibleSize);
  set_picky_constraints_v2(child, kIncompatibleSize);
  v2::BufferCollectionSetConstraintsRequest set_constraints_request;
  set_constraints_request.constraints().emplace();
  auto set_result = child2->SetConstraints(std::move(set_constraints_request));
  ASSERT_TRUE(set_result.is_ok());

  auto check_wait_result = [](decltype(root->WaitForAllBuffersAllocated())& result) {
    EXPECT_FALSE(result.is_ok());
    auto& error = result.error_value();
    EXPECT_TRUE(error.is_framework_error() &&
                    error.framework_error().status() == ZX_ERR_PEER_CLOSED ||
                error.is_domain_error() && error.domain_error() == ZX_ERR_NOT_SUPPORTED);
  };

  auto root_result = root->WaitForAllBuffersAllocated();
  check_wait_result(root_result);
  auto child_result = child->WaitForAllBuffersAllocated();
  check_wait_result(child_result);
  auto child2_result = child2->WaitForAllBuffersAllocated();
  check_wait_result(child2_result);

  // Make sure sysmem is still alive.
  auto check_token = create_initial_token_v2();
  auto sync_result = check_token->Sync();
  ASSERT_TRUE(sync_result.is_ok());
}

TEST(Sysmem, PixelFormatDoNotCare_SuccessV2) {
  auto token1 = create_initial_token_v2();
  auto token2 = create_token_under_token_v2(token1);

  auto collection1 = convert_token_to_collection_v2(std::move(token1));
  auto collection2 = convert_token_to_collection_v2(std::move(token2));

  v2::BufferCollectionConstraints c2;
  c2.usage().emplace();
  c2.usage()->cpu() = fuchsia_sysmem::kCpuUsageWriteOften;
  c2.min_buffer_count_for_camping() = 1;
  c2.buffer_memory_constraints().emplace();
  c2.buffer_memory_constraints()->min_size_bytes() = 1;
  c2.buffer_memory_constraints()->max_size_bytes() = 512 * 1024 * 1024;
  c2.image_format_constraints().emplace(1);
  c2.image_format_constraints()->at(0) = {};
  c2.image_format_constraints()->at(0).color_spaces().emplace(1);
  c2.image_format_constraints()->at(0).color_spaces()->at(0) = fuchsia_images2::ColorSpace::kRec709;
  c2.image_format_constraints()->at(0).min_size().emplace();
  c2.image_format_constraints()->at(0).min_size()->width() = 32;
  c2.image_format_constraints()->at(0).min_size()->height() = 32;

  // clone / copy
  auto c1 = c2;

  c1.image_format_constraints()->at(0).pixel_format() = fuchsia_images2::PixelFormat::kDoNotCare;
  c2.image_format_constraints()->at(0).pixel_format() = fuchsia_images2::PixelFormat::kNv12;

  EXPECT_FALSE(c1.image_format_constraints()->at(0).pixel_format_modifier().has_value());
  EXPECT_FALSE(c2.image_format_constraints()->at(0).pixel_format_modifier().has_value());

  auto set_verbose_result = collection1->SetVerboseLogging();
  ASSERT_TRUE(set_verbose_result.is_ok());

  v2::BufferCollectionSetConstraintsRequest set_constraints_request;
  set_constraints_request.constraints() = std::move(c1);
  auto set_result1 = collection1->SetConstraints(std::move(set_constraints_request));
  ASSERT_TRUE(set_result1.is_ok());

  v2::BufferCollectionSetConstraintsRequest set_constraints_request2;
  set_constraints_request2.constraints() = std::move(c2);
  auto set_result2 = collection2->SetConstraints(std::move(set_constraints_request2));
  ASSERT_TRUE(set_result2.is_ok());

  auto wait_result1 = collection1->WaitForAllBuffersAllocated();
  auto wait_result2 = collection2->WaitForAllBuffersAllocated();

  ASSERT_TRUE(wait_result1.is_ok());
  ASSERT_TRUE(wait_result2.is_ok());

  ASSERT_EQ(2, wait_result1->buffer_collection_info()->buffers()->size());
  ASSERT_EQ(fuchsia_images2::PixelFormat::kNv12, wait_result1->buffer_collection_info()
                                                     ->settings()
                                                     ->image_format_constraints()
                                                     ->pixel_format());
}

TEST(Sysmem, PixelFormatDoNotCare_MultiplePixelFormatsFailsV2) {
  // pass 0 - verify success when single pixel format
  // pass 1 - verify failure when multiple pixel formats
  // pass 1 - verify failure when multiple pixel formats (kInvalid variant)
  for (uint32_t pass = 0; pass < 3; ++pass) {
    auto token1 = create_initial_token_v2();
    auto token2 = create_token_under_token_v2(token1);

    auto collection1 = convert_token_to_collection_v2(std::move(token1));
    auto collection2 = convert_token_to_collection_v2(std::move(token2));

    v2::BufferCollectionConstraints c2;
    c2.usage().emplace();
    c2.usage()->cpu() = v2::kCpuUsageWriteOften;
    c2.min_buffer_count_for_camping() = 1;
    c2.image_format_constraints().emplace(1);
    c2.image_format_constraints()->at(0).color_spaces().emplace(1);
    c2.image_format_constraints()->at(0).color_spaces()->at(0) =
        fuchsia_images2::ColorSpace::kRec709;
    c2.image_format_constraints()->at(0).min_size().emplace();
    c2.image_format_constraints()->at(0).min_size()->width() = 32;
    c2.image_format_constraints()->at(0).min_size()->height() = 32;

    // clone / copy
    auto c1 = c2;

    // Setting two pixel_format values will intentionally cause failure.
    c1.image_format_constraints()->at(0).pixel_format() = fuchsia_images2::PixelFormat::kDoNotCare;
    if (pass != 0) {
      c1.image_format_constraints()->resize(2);
      c1.image_format_constraints()->at(1) = c1.image_format_constraints()->at(0);
      switch (pass) {
        case 1:
          c1.image_format_constraints()->at(1).pixel_format() = fuchsia_images2::PixelFormat::kNv12;
          break;
        case 2:
          c1.image_format_constraints()->at(1).pixel_format() =
              fuchsia_images2::PixelFormat::kInvalid;
          break;
      }
    }

    c2.image_format_constraints()->at(0).pixel_format() = fuchsia_images2::PixelFormat::kNv12;

    v2::BufferCollectionSetConstraintsRequest set_constraints_request;
    set_constraints_request.constraints() = std::move(c2);
    auto set_result2 = collection2->SetConstraints(std::move(set_constraints_request));
    ASSERT_TRUE(set_result2.is_ok());

    v2::BufferCollectionSetConstraintsRequest set_constraints_request2;
    set_constraints_request2.constraints() = std::move(c1);
    auto set_result1 = collection1->SetConstraints(std::move(set_constraints_request2));
    ASSERT_TRUE(set_result1.is_ok());

    auto wait_result1 = collection1->WaitForAllBuffersAllocated();
    auto wait_result2 = collection2->WaitForAllBuffersAllocated();

    if (pass == 0) {
      ASSERT_TRUE(wait_result1.is_ok());
      ASSERT_TRUE(wait_result2.is_ok());
      ASSERT_EQ(fuchsia_images2::PixelFormat::kNv12, wait_result1->buffer_collection_info()
                                                         ->settings()
                                                         ->image_format_constraints()
                                                         ->pixel_format()
                                                         .value());
    } else {
      ASSERT_FALSE(wait_result1.is_ok());
      ASSERT_FALSE(wait_result2.is_ok());
    }
  }
}

TEST(Sysmem, PixelFormatDoNotCare_UnconstrainedFailsV2) {
  // pass 0 - verify success when not all participants are unconstrained pixel format
  // pass 1 - verify fialure when all participants are unconstrained pixel format
  for (uint32_t pass = 0; pass < 2; ++pass) {
    auto token1 = create_initial_token_v2();
    auto token2 = create_token_under_token_v2(token1);

    auto collection1 = convert_token_to_collection_v2(std::move(token1));
    auto collection2 = convert_token_to_collection_v2(std::move(token2));

    v2::BufferCollectionConstraints c2;
    c2.usage().emplace();
    c2.usage()->cpu() = v2::kCpuUsageWriteOften;
    c2.min_buffer_count_for_camping() = 1;
    c2.image_format_constraints().emplace(1);
    c2.image_format_constraints()->at(0).color_spaces().emplace(1);
    c2.image_format_constraints()->at(0).color_spaces()->at(0) =
        fuchsia_images2::ColorSpace::kRec709;
    c2.image_format_constraints()->at(0).min_size().emplace();
    c2.image_format_constraints()->at(0).min_size()->width() = 32;
    c2.image_format_constraints()->at(0).min_size()->height() = 32;

    // clone / copy
    auto c1 = c2;

    c1.image_format_constraints()->at(0).pixel_format() = fuchsia_images2::PixelFormat::kDoNotCare;

    switch (pass) {
      case 0:
        c2.image_format_constraints()->at(0).pixel_format() = fuchsia_images2::PixelFormat::kNv12;
        break;
      case 1:
        // Not constraining the pixel format overall will intentionally cause failure.
        c2.image_format_constraints()->at(0).pixel_format() =
            fuchsia_images2::PixelFormat::kDoNotCare;
        break;
    }

    v2::BufferCollectionSetConstraintsRequest set_constraints_request;
    set_constraints_request.constraints() = std::move(c1);
    auto set_result1 = collection1->SetConstraints(std::move(set_constraints_request));
    ASSERT_TRUE(set_result1.is_ok());

    v2::BufferCollectionSetConstraintsRequest set_constraints_request2;
    set_constraints_request2.constraints() = std::move(c2);
    auto set_result2 = collection2->SetConstraints(std::move(set_constraints_request2));
    ASSERT_TRUE(set_result2.is_ok());

    auto wait_result1 = collection1->WaitForAllBuffersAllocated();
    auto wait_result2 = collection2->WaitForAllBuffersAllocated();

    if (pass == 0) {
      ASSERT_TRUE(wait_result1.is_ok());
      ASSERT_TRUE(wait_result2.is_ok());
      ASSERT_EQ(fuchsia_images2::PixelFormat::kNv12, wait_result1->buffer_collection_info()
                                                         ->settings()
                                                         ->image_format_constraints()
                                                         ->pixel_format()
                                                         .value());
    } else {
      ASSERT_FALSE(wait_result1.is_ok());
      ASSERT_FALSE(wait_result2.is_ok());
    }
  }
}

TEST(Sysmem, ColorSpaceDoNotCare_SuccessV2) {
  auto token1 = create_initial_token_v2();
  auto token2 = create_token_under_token_v2(token1);

  auto collection1 = convert_token_to_collection_v2(std::move(token1));
  auto collection2 = convert_token_to_collection_v2(std::move(token2));

  v2::BufferCollectionConstraints c2;
  c2.usage().emplace();
  c2.usage()->cpu() = v2::kCpuUsageWriteOften;
  c2.min_buffer_count_for_camping() = 1;
  c2.image_format_constraints().emplace(1);
  c2.image_format_constraints()->at(0).pixel_format() = fuchsia_images2::PixelFormat::kNv12;
  c2.image_format_constraints()->at(0).min_size().emplace();
  c2.image_format_constraints()->at(0).min_size()->width() = 32;
  c2.image_format_constraints()->at(0).min_size()->height() = 32;

  // clone / copy
  auto c1 = c2;

  c1.image_format_constraints()->at(0).color_spaces().emplace(1);
  c1.image_format_constraints()->at(0).color_spaces()->at(0) =
      fuchsia_images2::ColorSpace::kDoNotCare;

  c2.image_format_constraints()->at(0).color_spaces().emplace(1);
  c2.image_format_constraints()->at(0).color_spaces()->at(0) = fuchsia_images2::ColorSpace::kRec709;

  v2::BufferCollectionSetConstraintsRequest set_constraints_request;
  set_constraints_request.constraints() = std::move(c1);
  auto set_result1 = collection1->SetConstraints(std::move(set_constraints_request));
  ASSERT_TRUE(set_result1.is_ok());

  v2::BufferCollectionSetConstraintsRequest set_constraints_request2;
  set_constraints_request2.constraints() = std::move(c2);
  auto set_result2 = collection2->SetConstraints(std::move(set_constraints_request2));
  ASSERT_TRUE(set_result2.is_ok());

  auto wait_result1 = collection1->WaitForAllBuffersAllocated();
  auto wait_result2 = collection2->WaitForAllBuffersAllocated();

  ASSERT_TRUE(wait_result1.is_ok());
  ASSERT_TRUE(wait_result2.is_ok());

  ASSERT_EQ(2, wait_result1->buffer_collection_info()->buffers()->size());
  ASSERT_EQ(fuchsia_images2::PixelFormat::kNv12, wait_result1->buffer_collection_info()
                                                     ->settings()
                                                     ->image_format_constraints()
                                                     ->pixel_format()
                                                     .value());
  ASSERT_EQ(fuchsia_images2::ColorSpace::kRec709, wait_result1->buffer_collection_info()
                                                      ->settings()
                                                      ->image_format_constraints()
                                                      ->color_spaces()
                                                      ->at(0));
}

TEST(Sysmem, PixelFormatDoNotCare_OneColorSpaceElseFailsV2) {
  // In pass 0, c1 sets kDoNotCare (via ForceUnsetColorSpaceConstraint) and correctly sets a single
  // kInvalid color space - in this pass we expect success.
  //
  // In pass 1, c1 sets kDoNotCare (via ForceUnsetColorSpaceConstraints) but incorrectly sets more
  // than 1 color space - in this pass we expect failure.
  //
  // Pass 2 is a variant of pass 1 that sets the 2nd color space to kInvalid instead.
  for (uint32_t pass = 0; pass < 3; ++pass) {
    auto token1 = create_initial_token_v2();
    auto token2 = create_token_under_token_v2(token1);

    auto collection1 = convert_token_to_collection_v2(std::move(token1));
    auto collection2 = convert_token_to_collection_v2(std::move(token2));

    v2::BufferCollectionConstraints c2;
    c2.usage().emplace();
    c2.usage()->cpu() = v2::kCpuUsageWriteOften;
    c2.min_buffer_count_for_camping() = 1;
    c2.image_format_constraints().emplace(1);
    c2.image_format_constraints()->at(0).pixel_format() = fuchsia_images2::PixelFormat::kNv12;
    c2.image_format_constraints()->at(0).min_size().emplace();
    c2.image_format_constraints()->at(0).min_size()->width() = 32;
    c2.image_format_constraints()->at(0).min_size()->height() = 32;

    // clone / copy
    auto c1 = c2;

    // Setting >= 2 color spaces where at least one is kDoNotCare will trigger failure, regardless
    // of what the specific additional color space(s) are.
    switch (pass) {
      case 0:
        c1.image_format_constraints()->at(0).color_spaces().emplace(1);
        c1.image_format_constraints()->at(0).color_spaces()->at(0) =
            fuchsia_images2::ColorSpace::kDoNotCare;
        break;
      case 1:
        c1.image_format_constraints()->at(0).color_spaces().emplace(2);
        c1.image_format_constraints()->at(0).color_spaces()->at(0) =
            fuchsia_images2::ColorSpace::kDoNotCare;
        c1.image_format_constraints()->at(0).color_spaces()->at(1) =
            fuchsia_images2::ColorSpace::kRec709;
        break;
      case 2:
        c1.image_format_constraints()->at(0).color_spaces().emplace(2);
        c1.image_format_constraints()->at(0).color_spaces()->at(0) =
            fuchsia_images2::ColorSpace::kInvalid;
        c1.image_format_constraints()->at(0).color_spaces()->at(1) =
            fuchsia_images2::ColorSpace::kDoNotCare;
        break;
    }

    c2.image_format_constraints()->at(0).color_spaces().emplace(1);
    c2.image_format_constraints()->at(0).color_spaces()->at(0) =
        fuchsia_images2::ColorSpace::kRec709;

    v2::BufferCollectionSetConstraintsRequest set_constraints_request;
    set_constraints_request.constraints() = std::move(c1);
    auto set_result1 = collection1->SetConstraints(std::move(set_constraints_request));
    ASSERT_TRUE(set_result1.is_ok());

    v2::BufferCollectionSetConstraintsRequest set_constraints_request2;
    set_constraints_request2.constraints() = std::move(c2);
    auto set_result2 = collection2->SetConstraints(std::move(set_constraints_request2));
    ASSERT_TRUE(set_result2.is_ok());

    auto wait_result1 = collection1->WaitForAllBuffersAllocated();
    auto wait_result2 = collection2->WaitForAllBuffersAllocated();

    if (pass == 0) {
      ASSERT_TRUE(wait_result1.is_ok());
      ASSERT_TRUE(wait_result2.is_ok());
      ASSERT_EQ(fuchsia_images2::ColorSpace::kRec709, wait_result1->buffer_collection_info()
                                                          ->settings()
                                                          ->image_format_constraints()
                                                          ->color_spaces()
                                                          ->at(0));
    } else {
      ASSERT_FALSE(wait_result1.is_ok());
      ASSERT_FALSE(wait_result2.is_ok());
    }
  }
}

TEST(Sysmem, ColorSpaceDoNotCare_UnconstrainedColorSpaceRemovesPixelFormatV2) {
  // In pass 0, we verify that with a participant (2) that does constrain the color space, success.
  //
  // In pass 1, we verify that essentially "removing" the constraining participant (2) is enough to
  // cause a failure, despite participant 1 setting the same constraints it did in pass 0 (including
  // sending ForceUnsetColorSpaceConstraint()).
  //
  // This way, we know that the reason for the failure seen by participant 1 in pass 1 is the lack
  // of any participant that constrains the color space, not just a problem with constraints set by
  // participant 1 (in both passes).
  for (uint32_t pass = 0; pass < 2; ++pass) {
    // All the "decltype(1)" stuff is defined in both passes but only actually used in pass 0, not
    // pass 1.

    auto token1 = create_initial_token_v2();
    decltype(token1) token2;
    if (pass == 0) {
      token2 = create_token_under_token_v2(token1);
    }

    auto collection1 = convert_token_to_collection_v2(std::move(token1));
    decltype(collection1) collection2;
    if (pass == 0) {
      collection2 = convert_token_to_collection_v2(std::move(token2));
    }

    v2::BufferCollectionConstraints c1;
    c1.usage().emplace();
    c1.usage()->cpu() = v2::kCpuUsageWriteOften;
    c1.min_buffer_count_for_camping() = 1;
    c1.image_format_constraints().emplace(1);
    c1.image_format_constraints()->at(0).pixel_format() = fuchsia_images2::PixelFormat::kNv12;
    // c1 is logically kDoNotCare, via ForceUnsetColorSpaceConstraint() sent below.  This field is
    // copied but then overridden for c2 (pass 0 only).
    c1.image_format_constraints()->at(0).min_size().emplace();
    c1.image_format_constraints()->at(0).min_size()->width() = 32;
    c1.image_format_constraints()->at(0).min_size()->height() = 32;

    // clone / copy
    auto c2 = c1;
    c1.image_format_constraints()->at(0).color_spaces().emplace(1);
    c1.image_format_constraints()->at(0).color_spaces()->at(0) =
        fuchsia_images2::ColorSpace::kDoNotCare;
    if (pass == 0) {
      // c2 will constrain the pixel format (by setting a valid one here and not sending
      // ForceUnsetColorSpaceConstraint() below).
      c2.image_format_constraints()->at(0).color_spaces().emplace(1);
      c2.image_format_constraints()->at(0).color_spaces()->at(0) =
          fuchsia_images2::ColorSpace::kRec709;
    }

    v2::BufferCollectionSetConstraintsRequest set_constraints_request;
    set_constraints_request.constraints() = std::move(c1);
    auto set_result1 = collection1->SetConstraints(std::move(set_constraints_request));
    ASSERT_TRUE(set_result1.is_ok());
    if (pass == 0) {
      v2::BufferCollectionSetConstraintsRequest set_constraints_request2;
      set_constraints_request2.constraints() = std::move(c2);
      auto set_result2 = collection2->SetConstraints(std::move(set_constraints_request2));
      ASSERT_TRUE(set_result2.is_ok());
    }

    auto wait_result1 = collection1->WaitForAllBuffersAllocated();
    if (pass == 0) {
      // Expected success because c2's constraining of the color space allows for c1's lack of
      // constraining of the color space.
      ASSERT_TRUE(wait_result1.is_ok());
      auto wait_result2 = collection2->WaitForAllBuffersAllocated();
      ASSERT_TRUE(wait_result2.is_ok());
      // may as well check that the color space is indeed kRec709, but this is also checked by the
      // _Success test above.
      ASSERT_EQ(fuchsia_images2::ColorSpace::kRec709, wait_result1->buffer_collection_info()
                                                          ->settings()
                                                          ->image_format_constraints()
                                                          ->color_spaces()
                                                          ->at(0));
    } else {
      // Expect failed because c2's constraining of the color space is missing in pass 1, leaving
      // the color space completely unconstrained.  By design, sysmem doesn't arbitrarily select a
      // color space when the color space is completely unconstrained (at least for now).
      ASSERT_FALSE(wait_result1.is_ok());
    }
  }
}
