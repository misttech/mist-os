// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fuchsia/component/cpp/fidl.h>
#include <fuchsia/component/decl/cpp/fidl.h>
#include <lib/async/cpp/executor.h>
#include <lib/diagnostics/reader/cpp/archive_reader.h>
#include <lib/fdio/directory.h>
#include <lib/fdio/fd.h>
#include <lib/fdio/fdio.h>
#include <lib/sys/cpp/component_context.h>
#include <stdlib.h>

#include <gmock/gmock.h>
#include <src/lib/files/file.h>
#include <src/lib/files/glob.h>

#include "src/lib/testing/loop_fixture/real_loop_fixture.h"
#include "third_party/rapidjson/include/rapidjson/rapidjson.h"

using diagnostics::reader::InspectData;

constexpr char kTestCollectionName[] = "test_apps";
constexpr char kTestChildUrl[] = "#meta/memory_monitor_test_app.cm";

class InspectTest : public gtest::RealLoopFixture {
 protected:
  InspectTest()
      : context_(sys::ComponentContext::Create()),
        child_name_(::testing::UnitTest::GetInstance()->current_test_info()->name()) {
    context_->svc()->Connect(realm_proxy_.NewRequest());
    StartChild();
  }

  ~InspectTest() { DestroyChild(); }

  std::string ChildName() { return std::string(kTestCollectionName) + ":" + child_name_; }

  std::string ChildSelector() {
    return diagnostics::reader::SanitizeMonikerForSelectors(ChildName()) + ":root";
  }

  fuchsia::component::decl::ChildRef ChildRef() {
    return {
        .name = child_name_,
        .collection = kTestCollectionName,
    };
  }

  void StartChild() {
    fuchsia::component::decl::CollectionRef collection_ref = {
        .name = kTestCollectionName,
    };
    fuchsia::component::decl::Child child_decl;
    child_decl.set_name(child_name_);
    child_decl.set_url(kTestChildUrl);
    child_decl.set_startup(fuchsia::component::decl::StartupMode::LAZY);

    realm_proxy_->CreateChild(std::move(collection_ref), std::move(child_decl),
                              fuchsia::component::CreateChildArgs(),
                              [&](fuchsia::component::Realm_CreateChild_Result result) {
                                ZX_ASSERT(!result.is_err());
                                ConnectChildBinder();
                              });
  }

  void ConnectChildBinder() {
    fidl::InterfaceHandle<fuchsia::io::Directory> exposed_dir;
    realm_proxy_->OpenExposedDir(
        ChildRef(), exposed_dir.NewRequest(),
        [exposed_dir = std::move(exposed_dir)](
            fuchsia::component::Realm_OpenExposedDir_Result result) mutable {
          ZX_ASSERT(!result.is_err());
          std::shared_ptr<sys::ServiceDirectory> svc = std::make_shared<sys::ServiceDirectory>(
              sys::ServiceDirectory(std::move(exposed_dir)));

          fuchsia::component::BinderPtr binder;
          svc->Connect(binder.NewRequest());
        });
  }

  void DestroyChild() {
    auto destroyed = false;
    realm_proxy_->DestroyChild(ChildRef(),
                               [&](fuchsia::component::Realm_DestroyChild_Result result) {
                                 ZX_ASSERT(!result.is_err());
                                 destroyed = true;
                               });
    RunLoopUntil([&destroyed] { return destroyed; });

    // make the child name unique so we don't snapshot inspect from the first instance accidentally
    child_name_ += "1";
  }

  fpromise::result<InspectData> GetInspect() {
    diagnostics::reader::ArchiveReader reader(dispatcher(), {ChildSelector()});
    fpromise::result<std::vector<InspectData>, std::string> result;
    async::Executor executor(dispatcher());
    executor.schedule_task(
        reader.SnapshotInspectUntilPresent({ChildName()})
            .then([&](fpromise::result<std::vector<InspectData>, std::string>& rest) {
              result = std::move(rest);
            }));
    RunLoopUntil([&] { return result.is_ok() || result.is_error(); });

    if (result.is_error()) {
      EXPECT_FALSE(result.is_error()) << "Error was " << result.error();
      return fpromise::error();
    }

    if (result.value().size() != 1) {
      EXPECT_EQ(1u, result.value().size()) << "Expected only one component";
      return fpromise::error();
    }

    return fpromise::ok(std::move(result.value()[0]));
  }

 private:
  std::unique_ptr<sys::ComponentContext> context_;
  std::string child_name_;
  fuchsia::component::RealmPtr realm_proxy_;
};

void expect_array_non_empty(const InspectData& data, const std::vector<std::string>& path) {
  auto& value = data.GetByPath(path);
  EXPECT_EQ(value.GetType(), rapidjson::kArrayType) << path.back() << " is not an array";
  EXPECT_NE(value.GetArray().Size(), 0u) << path.back() << " is empty";
}

void expect_number(const InspectData& data, const std::vector<std::string>& path) {
  auto& value = data.GetByPath(path);
  EXPECT_EQ(value.GetType(), rapidjson::kNumberType) << path.back() << " is not a number";
}

void expect_string_not_empty(const InspectData& data, const std::vector<std::string>& path) {
  auto& value = data.GetByPath(path);
  EXPECT_EQ(value.GetType(), rapidjson::kStringType) << path.back() << " is not a string";
  EXPECT_NE(value.GetStringLength(), 0u) << path.back() << " is empty";
}

void expect_object_not_empty(const InspectData& data, const std::vector<std::string>& path) {
  auto& value = data.GetByPath(path);
  EXPECT_EQ(value.GetType(), rapidjson::kObjectType) << path.back() << " is not an object";
  EXPECT_FALSE(value.ObjectEmpty()) << path.back() << " is empty";
}

TEST_F(InspectTest, FirstLaunch) {
  auto result = GetInspect();
  ASSERT_TRUE(result.is_ok());
  auto data = result.take_value();
  expect_string_not_empty(data, {"root", "current"});
  expect_string_not_empty(data, {"root", "current_digest"});
  expect_string_not_empty(data, {"root", "high_water"});
  expect_string_not_empty(data, {"root", "high_water_digest"});
  expect_object_not_empty(data, {"root", "values"});
}

TEST_F(InspectTest, SecondLaunch) {
  // Make sure that the *_previous_boot properties are made visible only upon
  // the second run.
  auto result = GetInspect();
  ASSERT_TRUE(result.is_ok());
  auto data = result.take_value();
  expect_string_not_empty(data, {"root", "current"});
  expect_string_not_empty(data, {"root", "current_digest"});
  expect_string_not_empty(data, {"root", "high_water"});
  expect_string_not_empty(data, {"root", "high_water_digest"});
  expect_object_not_empty(data, {"root", "values"});

  DestroyChild();
  StartChild();

  result = GetInspect();
  ASSERT_TRUE(result.is_ok());
  data = result.take_value();
  expect_string_not_empty(data, {"root", "current"});
  expect_string_not_empty(data, {"root", "current_digest"});
  expect_string_not_empty(data, {"root", "high_water"});
  expect_string_not_empty(data, {"root", "high_water_previous_boot"});
  expect_string_not_empty(data, {"root", "high_water_digest"});
  expect_string_not_empty(data, {"root", "high_water_digest_previous_boot"});

  expect_object_not_empty(data, {"root", "values"});
  expect_number(data, {"root", "values", "total_bytes"});
  expect_number(data, {"root", "values", "free_bytes"});
  expect_number(data, {"root", "values", "wired_bytes"});
  expect_number(data, {"root", "values", "total_heap_bytes"});
  expect_number(data, {"root", "values", "free_heap_bytes"});
  expect_number(data, {"root", "values", "vmo_bytes"});
  expect_number(data, {"root", "values", "vmo_pager_total_bytes"});
  expect_number(data, {"root", "values", "vmo_pager_newest_bytes"});
  expect_number(data, {"root", "values", "vmo_pager_oldest_bytes"});
  expect_number(data, {"root", "values", "vmo_discardable_locked_bytes"});
  expect_number(data, {"root", "values", "vmo_discardable_unlocked_bytes"});
  expect_number(data, {"root", "values", "mmu_overhead_bytes"});
  expect_number(data, {"root", "values", "ipc_bytes"});
  expect_number(data, {"root", "values", "other_bytes"});
  expect_number(data, {"root", "values", "vmo_reclaim_disabled_bytes"});

  expect_object_not_empty(data, {"root", "kmem_stats_compression"});
  expect_number(data, {"root", "kmem_stats_compression", "uncompressed_storage_bytes"});
  expect_number(data, {"root", "kmem_stats_compression", "compressed_storage_bytes"});
  expect_number(data, {"root", "kmem_stats_compression", "compressed_fragmentation_bytes"});
  expect_number(data, {"root", "kmem_stats_compression", "compression_time"});
  expect_number(data, {"root", "kmem_stats_compression", "decompression_time"});
  expect_number(data, {"root", "kmem_stats_compression", "total_page_compression_attempts"});
  expect_number(data, {"root", "kmem_stats_compression", "failed_page_compression_attempts"});
  expect_number(data, {"root", "kmem_stats_compression", "total_page_decompressions"});
  expect_number(data, {"root", "kmem_stats_compression", "compressed_page_evictions"});
  expect_number(data, {"root", "kmem_stats_compression", "eager_page_compressions"});
  expect_number(data, {"root", "kmem_stats_compression", "critical_memory_page_compressions"});
  expect_number(data, {"root", "kmem_stats_compression", "pages_decompressed_unit_ns"});
  expect_array_non_empty(data,
                         {"root", "kmem_stats_compression", "pages_decompressed_within_log_time"});
}
