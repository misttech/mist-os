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
#include <lib/syslog/cpp/macros.h>
#include <stdlib.h>

#include <filesystem>

#include <gmock/gmock.h>
#include <rapidjson/ostreamwrapper.h>
#include <rapidjson/prettywriter.h>
#include <src/lib/files/file.h>
#include <src/lib/files/glob.h>

#include "src/lib/testing/loop_fixture/real_loop_fixture.h"
#include "third_party/rapidjson/include/rapidjson/rapidjson.h"

using ::diagnostics::reader::InspectData;
namespace {
constexpr char kTestCollectionName[] = "test_apps";
constexpr char kTestChildUrl[] = "#meta/memory_monitor_test_app.cm";

// This class manages the test application, wiring it up, starting it and
// collecting its inspect data.
class InspectTest : public gtest::RealLoopFixture {
 protected:
  InspectTest()
      : context_(sys::ComponentContext::Create()),
        child_name_(::testing::UnitTest::GetInstance()->current_test_info()->name()) {
    // Clear the persistent storage provided to the memory_monitor between tests so the
    // `*_previous_boot` information is not returned.
    for (std::filesystem::recursive_directory_iterator i("/cache"), end; i != end; ++i) {
      if (!is_directory(i->path())) {
        std::filesystem::remove(i->path());
      }
    }

    context_->svc()->Connect(realm_proxy_.NewRequest());
    StartChild();
  }

  ~InspectTest() override { DestroyChild(); }

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

MATCHER_P(EqJson, json_text, "") {
  rapidjson::Document expected;
  expected.Parse(json_text);
  return arg == expected;
}

MATCHER_P(IsObjectWithKeys, matcher,
          "Is an object with keys that " +
              testing::DescribeMatcher<std::vector<std::string>>(matcher, negation)) {
  std::vector<std::string> keys;
  for (auto i = arg.MemberBegin(); i != arg.MemberEnd(); ++i) {
    keys.push_back(i->name.GetString());
  }
  return ExplainMatchResult(testing::Eq(rapidjson::kObjectType), arg.GetType(), result_listener) &&
         ExplainMatchResult(matcher, keys, result_listener);
}

MATCHER_P(IsString, matcher,
          "Is a string that " + testing::DescribeMatcher<std::string>(matcher, negation)) {
  return ExplainMatchResult(testing::Eq(rapidjson::kStringType), arg.GetType(), result_listener) &&
         ExplainMatchResult(matcher, arg.GetString(), result_listener);
}
MATCHER_P(IsNumber, matcher,
          "Is a number that " + testing::DescribeMatcher<std::string>(matcher, negation)) {
  return ExplainMatchResult(testing::Eq(rapidjson::kNumberType), arg.GetType(), result_listener) &&
         ExplainMatchResult(matcher, arg.GetInt64(), result_listener);
}
}  // namespace

namespace rapidjson {
// Teach the testing framework to pretty print Json values.
extern void PrintTo(const Value& value, ::std::ostream* os) {
  OStreamWrapper osw(*os);
  PrettyWriter<OStreamWrapper> writer(osw);
  value.Accept(writer);
}

extern void PrintTo(const Document& value, ::std::ostream* os) {
  OStreamWrapper osw(*os);
  PrettyWriter<OStreamWrapper> writer(osw);
  value.Accept(writer);
}
}  // namespace rapidjson

TEST_F(InspectTest, FirstLaunch) {
  auto result = GetInspect();
  ASSERT_TRUE(result.is_ok());
  auto data = result.take_value();

  EXPECT_THAT(data.GetByPath({"root"}),
              IsObjectWithKeys(testing::UnorderedElementsAreArray(
                  {"logger", "current", "high_water", "current_digest", "high_water_digest",
                   "kmem_stats_compression", "values"})));
}

TEST_F(InspectTest, SecondLaunch) {
  // Make sure that the *_previous_boot properties are made visible only upon
  // the second run.
  auto result = GetInspect();
  ASSERT_TRUE(result.is_ok());
  auto data = result.take_value();

  EXPECT_THAT(data.GetByPath({"root"}),
              IsObjectWithKeys(testing::UnorderedElementsAreArray(
                  {"logger", "current", "high_water", "current_digest", "high_water_digest",
                   "kmem_stats_compression", "values"})));

  DestroyChild();
  StartChild();

  result = GetInspect();
  ASSERT_TRUE(result.is_ok());
  data = result.take_value();

  EXPECT_THAT(data.GetByPath({"root"}),
              IsObjectWithKeys(testing::UnorderedElementsAreArray(
                  {"logger", "current", "high_water", "high_water_previous_boot", "current_digest",
                   "high_water_digest", "high_water_digest_previous_boot", "kmem_stats_compression",
                   "values"})));
  EXPECT_THAT(data.GetByPath({"root", "current"}), EqJson(R"json(
    "Time: 0 VMO: 46B Free: 41B\nkernel<1> 280B\n other 52B\n ipc 48B\n mmu 47B\n vmo 46B\n heap 44B\n wired 43B\n"
  )json"));
  EXPECT_THAT(data.GetByPath({"root", "high_water"}), EqJson(R"json(
    "Time: 0 VMO: 46B Free: 41B\nkernel<1> 280B\n other 52B\n ipc 48B\n mmu 47B\n vmo 46B\n heap 44B\n wired 43B\n"
  )json"));
  EXPECT_THAT(data.GetByPath({"root", "high_water_previous_boot"}), EqJson(R"json(
    "Time: 0 VMO: 46B Free: 41B\nkernel<1> 280B\n other 52B\n ipc 48B\n mmu 47B\n vmo 46B\n heap 44B\n wired 43B\n"
  )json"));
  EXPECT_THAT(data.GetByPath({"root", "current_digest"}), EqJson(R"json(
    "Kernel: 234B\n[Addl]ZramCompressedBytes: 61B\n[Addl]DiscardableUnlocked: 58B\n[Addl]DiscardableLocked: 57B\n[Addl]PagerOldest: 55B\n[Addl]PagerNewest: 54B\n[Addl]PagerTotal: 53B\nOrphaned: 46B\nFree: 41B\nUndigested: 0B\n"
  )json"));
  EXPECT_THAT(data.GetByPath({"root", "high_water_digest"}), EqJson(R"json(
    "Kernel: 234B\n[Addl]ZramCompressedBytes: 61B\n[Addl]DiscardableUnlocked: 58B\n[Addl]DiscardableLocked: 57B\n[Addl]PagerOldest: 55B\n[Addl]PagerNewest: 54B\n[Addl]PagerTotal: 53B\nOrphaned: 46B\nFree: 41B\nUndigested: 0B\n"
  )json"));
  EXPECT_THAT(
      data.GetByPath({"root", "high_water_digest_previous_boot"}),
      EqJson(
          R"json("Kernel: 234B\n[Addl]ZramCompressedBytes: 61B\n[Addl]DiscardableUnlocked: 58B\n[Addl]DiscardableLocked: 57B\n[Addl]PagerOldest: 55B\n[Addl]PagerNewest: 54B\n[Addl]PagerTotal: 53B\nOrphaned: 46B\nFree: 41B\nUndigested: 0B\n"
  )json"));

  EXPECT_THAT(data.GetByPath({"root", "values"}), EqJson(R"json(
    {
      "total_bytes": 40,
      "free_bytes": 41,
      "wired_bytes": 43,
      "total_heap_bytes": 44,
      "free_heap_bytes": 45,
      "vmo_bytes": 46,
      "vmo_pager_total_bytes": 53,
      "vmo_pager_newest_bytes": 54,
      "vmo_pager_oldest_bytes": 55,
      "vmo_discardable_locked_bytes": 57,
      "vmo_discardable_unlocked_bytes": 58,
      "mmu_overhead_bytes": 47,
      "ipc_bytes": 48,
      "other_bytes": 52,
      "vmo_reclaim_disabled_bytes": 56
    }
  )json"));

  EXPECT_THAT(data.GetByPath({"root", "kmem_stats_compression"}), EqJson(R"json(
    {
      "uncompressed_storage_bytes": 60,
      "compressed_storage_bytes": 61,
      "compressed_fragmentation_bytes": 62,
      "compression_time": 63,
      "decompression_time": 64,
      "total_page_compression_attempts": 65,
      "failed_page_compression_attempts": 66,
      "total_page_decompressions": 67,
      "compressed_page_evictions": 68,
      "eager_page_compressions": 69,
      "memory_pressure_page_compressions": 70,
      "critical_memory_page_compressions": 71,
      "pages_decompressed_unit_ns": 72,
      "pages_decompressed_within_log_time": [
        73,
        74,
        75,
        76,
        77,
        78,
        79,
        80
      ]
    }
  )json"));
}
