// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/fit/function.h>
#include <lib/maybe-standalone-test/maybe-standalone.h>
#include <lib/zx/job.h>
#include <lib/zx/process.h>
#include <lib/zx/thread.h>
#include <lib/zx/vmar.h>
#include <lib/zx/vmo.h>
#include <zircon/syscalls-next.h>

#include <memory>
#include <set>
#include <string_view>
#include <utility>

#include <fbl/vector.h>
#include <zxtest/zxtest.h>

#include "helper.h"

namespace object_info_test {
namespace {

// ZX_INFO_MEMORY_ATTRIBUTION tests

// Test fixture that skips the test if the current kernel was not compiled with kernel-based memory
// attribution support.
class MemoryAttributionTest : public zxtest::Test {
 public:
  void SetUp() override {
    zx_status_t probe =
        zx::process::self()->get_info(ZX_INFO_MEMORY_ATTRIBUTION, nullptr, 0, nullptr, nullptr);
    if (probe == ZX_ERR_NOT_SUPPORTED)
      ZXTEST_SKIP("Kernel-based memory attribution is not available in this kernel");
  }
};

// Test fixture for tests that operate on a static hierarchy of jobs and processes.
//
// It instantiates the following hierarchy:
// - job_a
//   - process_a1
//   - process_a2
//   - job_b
//     - process_b
//     - job_c
//       - process_c
//   - job_d
//     - process_d
class MemoryAttributionImmutableHierarchyTest : public MemoryAttributionTest {
 public:
  static void SetUpTestSuite() {
    handles_ = std::make_unique<handles>();
    zx::vmar scratch;

    // Job A
    ASSERT_OK(zx::job::create(*zx::job::default_job(), 0, &handles_->job_a));
    ASSERT_OK(zx::process::create(handles_->job_a, "a1", 2, 0, &handles_->process_a1, &scratch));
    ASSERT_OK(zx::process::create(handles_->job_a, "a2", 2, 0, &handles_->process_a2, &scratch));

    // Job B
    ASSERT_OK(zx::job::create(handles_->job_a, 0, &handles_->job_b));
    ASSERT_OK(zx::process::create(handles_->job_b, "b", 1, 0, &handles_->process_b, &scratch));

    // Job C
    ASSERT_OK(zx::job::create(handles_->job_b, 0, &handles_->job_c));
    ASSERT_OK(zx::process::create(handles_->job_c, "c", 1, 0, &handles_->process_c, &scratch));

    // Job D
    ASSERT_OK(zx::job::create(handles_->job_a, 0, &handles_->job_d));
    ASSERT_OK(zx::process::create(handles_->job_d, "d", 1, 0, &handles_->process_d, &scratch));
  }

  // Close all the handles.
  static void TearDownTestSuite() { handles_.reset(); }

  static const zx::job& GetJobA() { return handles_->job_a; }
  static const zx::job& GetJobB() { return handles_->job_b; }
  static const zx::job& GetJobC() { return handles_->job_c; }
  static const zx::job& GetJobD() { return handles_->job_d; }

  static const zx::process& GetProcessA1() { return handles_->process_a1; }
  static const zx::process& GetProcessA2() { return handles_->process_a2; }
  static const zx::process& GetProcessB() { return handles_->process_b; }
  static const zx::process& GetProcessC() { return handles_->process_c; }
  static const zx::process& GetProcessD() { return handles_->process_d; }

 private:
  struct handles {
    zx::job job_a, job_b, job_c, job_d;
    zx::process process_a1, process_a2, process_b, process_c, process_d;
  };

  static std::unique_ptr<handles> handles_;
};

std::unique_ptr<MemoryAttributionImmutableHierarchyTest::handles>
    MemoryAttributionImmutableHierarchyTest::handles_;

// Returns the koid of the given object.
zx_koid_t GetKoid(const zx::object_base& object) {
  if (!object.is_valid()) {
    return ZX_KOID_INVALID;
  }

  zx_info_handle_basic_t info;
  ZX_ASSERT(object.get_info(ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr) == ZX_OK);
  return info.koid;
}

// Returns the koid of the parent job.
zx_koid_t GetParentJobKoid(const zx::unowned_job& job) {
  if (!job->is_valid()) {
    return ZX_KOID_INVALID;
  }

  zx_info_handle_basic_t info;
  ZX_ASSERT(job->get_info(ZX_INFO_HANDLE_BASIC, &info, sizeof(info), nullptr, nullptr) == ZX_OK);
  return info.related_koid;
}

// Enumerate all the koids in the zx_info_memory_attribution_t entries.
void EnumerateVisibleKoids(const zx::object_base& object, std::set<zx_koid_t>& result) {
  size_t actual, avail;
  ASSERT_OK(object.get_info(ZX_INFO_MEMORY_ATTRIBUTION, nullptr, 0, nullptr, &avail));

  std::vector<zx_info_memory_attribution_t> buf(avail);
  ASSERT_OK(object.get_info(ZX_INFO_MEMORY_ATTRIBUTION, buf.data(),
                            buf.size() * sizeof(zx_info_memory_attribution_t), &actual, &avail));
  ASSERT_EQ(actual, buf.size());
  ASSERT_EQ(avail, buf.size());

  result.clear();
  for (const zx_info_memory_attribution_t& entry : buf) {
    bool inserted = result.insert(entry.process_koid).second;
    EXPECT_TRUE(inserted, "Duplicate koid detected");
  }
}

// Verifies that each process handle gives access to the corresponding process only.
TEST_F(MemoryAttributionImmutableHierarchyTest, InfoMemoryAttributionProcessVisibility) {
  std::set<zx_koid_t> visible_koids_a1;
  ASSERT_NO_FATAL_FAILURE((EnumerateVisibleKoids(GetProcessA1(), visible_koids_a1)));
  EXPECT_TRUE(visible_koids_a1 == std::set<zx_koid_t>{GetKoid(GetProcessA1())});

  std::set<zx_koid_t> visible_koids_a2;
  ASSERT_NO_FATAL_FAILURE((EnumerateVisibleKoids(GetProcessA2(), visible_koids_a2)));
  EXPECT_TRUE(visible_koids_a2 == std::set<zx_koid_t>{GetKoid(GetProcessA2())});

  std::set<zx_koid_t> visible_koids_b;
  ASSERT_NO_FATAL_FAILURE((EnumerateVisibleKoids(GetProcessB(), visible_koids_b)));
  EXPECT_TRUE(visible_koids_b == std::set<zx_koid_t>{GetKoid(GetProcessB())});

  std::set<zx_koid_t> visible_koids_c;
  ASSERT_NO_FATAL_FAILURE((EnumerateVisibleKoids(GetProcessC(), visible_koids_c)));
  EXPECT_TRUE(visible_koids_c == std::set<zx_koid_t>{GetKoid(GetProcessC())});

  std::set<zx_koid_t> visible_koids_d;
  ASSERT_NO_FATAL_FAILURE((EnumerateVisibleKoids(GetProcessD(), visible_koids_d)));
  EXPECT_TRUE(visible_koids_d == std::set<zx_koid_t>{GetKoid(GetProcessD())});
}

// Verifies that each job handle gives access to the corresponding subprocesses only.
TEST_F(MemoryAttributionImmutableHierarchyTest, InfoMemoryAttributionJobVisibility) {
  std::set<zx_koid_t> visible_koids_a;
  ASSERT_NO_FATAL_FAILURE((EnumerateVisibleKoids(GetJobA(), visible_koids_a)));
  EXPECT_TRUE(
      (visible_koids_a == std::set<zx_koid_t>{GetKoid(GetProcessA1()), GetKoid(GetProcessA2()),
                                              GetKoid(GetProcessB()), GetKoid(GetProcessC()),
                                              GetKoid(GetProcessD())}));

  std::set<zx_koid_t> visible_koids_b;
  ASSERT_NO_FATAL_FAILURE((EnumerateVisibleKoids(GetJobB(), visible_koids_b)));
  EXPECT_TRUE(
      (visible_koids_b == std::set<zx_koid_t>{GetKoid(GetProcessB()), GetKoid(GetProcessC())}));

  std::set<zx_koid_t> visible_koids_c;
  ASSERT_NO_FATAL_FAILURE((EnumerateVisibleKoids(GetJobC(), visible_koids_c)));
  EXPECT_TRUE(visible_koids_c == std::set<zx_koid_t>{GetKoid(GetProcessC())});

  std::set<zx_koid_t> visible_koids_d;
  ASSERT_NO_FATAL_FAILURE((EnumerateVisibleKoids(GetJobD(), visible_koids_d)));
  EXPECT_TRUE(visible_koids_d == std::set<zx_koid_t>{GetKoid(GetProcessD())});
}

// Verifies that the root job contains the kernel's attribution object.
TEST_F(MemoryAttributionImmutableHierarchyTest,
       InfoMemoryAttributionRootJobContainsKernelAttributionObject) {
  // This test requires a handle to the root job, which is only available in standalone mode.
  bool is_running_standalone = maybe_standalone::GetBootOptions() != nullptr;
  if (!is_running_standalone) {
    ZXTEST_SKIP("This test is only meaninful in standalone mode");
  }
  zx::unowned_job root_job = zx::job::default_job();
  ASSERT_EQ(ZX_HANDLE_INVALID, GetParentJobKoid(root_job), "This must be the root job");

  std::set<zx_koid_t> visible_koids_root;
  ASSERT_NO_FATAL_FAILURE((EnumerateVisibleKoids(*root_job, visible_koids_root)));
  EXPECT_TRUE(visible_koids_root.find(ZX_KOID_INVALID) != visible_koids_root.end());
}

TEST_F(MemoryAttributionImmutableHierarchyTest, InfoMemoryAttributionInvalidHandleFails) {
  ASSERT_NO_FATAL_FAILURE((CheckInvalidHandleFails<zx_info_memory_attribution_t>(
      ZX_INFO_MEMORY_ATTRIBUTION, 1, GetProcessA1)));
}

TEST_F(MemoryAttributionImmutableHierarchyTest, InfoMemoryAttributionNullAvailSucceeds) {
  ASSERT_NO_FATAL_FAILURE((CheckNullAvailSuceeds<zx_info_memory_attribution_t>(
      ZX_INFO_MEMORY_ATTRIBUTION, 1, GetProcessA1)));
}

TEST_F(MemoryAttributionImmutableHierarchyTest, InfoMemoryAttributionNullActualSucceeds) {
  ASSERT_NO_FATAL_FAILURE((CheckNullActualSuceeds<zx_info_memory_attribution_t>(
      ZX_INFO_MEMORY_ATTRIBUTION, 1, GetProcessA1)));
}

TEST_F(MemoryAttributionImmutableHierarchyTest, InfoMemoryAttributionNullActualAndAvailSucceeds) {
  ASSERT_NO_FATAL_FAILURE((CheckNullActualAndAvailSuceeds<zx_info_memory_attribution_t>(
      ZX_INFO_MEMORY_ATTRIBUTION, 1, GetProcessA1)));
}

TEST_F(MemoryAttributionImmutableHierarchyTest, InfoMemoryAttributionInvalidBufferPointerFails) {
  ASSERT_NO_FATAL_FAILURE((CheckInvalidBufferPointerFails<zx_info_memory_attribution_t>(
      ZX_INFO_MEMORY_ATTRIBUTION, GetProcessA1)));
}

TEST_F(MemoryAttributionImmutableHierarchyTest, InfoMemoryAttributionBadActualIsInvalidArg) {
  ASSERT_NO_FATAL_FAILURE((BadActualIsInvalidArgs<zx_info_memory_attribution_t>(
      ZX_INFO_MEMORY_ATTRIBUTION, 1, GetProcessA1)));
}

TEST_F(MemoryAttributionImmutableHierarchyTest, InfoMemoryAttributionBadAvailIsInvalidArg) {
  ASSERT_NO_FATAL_FAILURE((BadAvailIsInvalidArgs<zx_info_memory_attribution_t>(
      ZX_INFO_MEMORY_ATTRIBUTION, 1, GetProcessA1)));
}

TEST_F(MemoryAttributionImmutableHierarchyTest, InfoMemoryAttributionZeroSizeBufferSucceeds) {
  ASSERT_NO_FATAL_FAILURE((CheckZeroSizeBufferSucceeds<zx_info_memory_attribution_t>(
      ZX_INFO_MEMORY_ATTRIBUTION, GetProcessA1)));
}

TEST_F(MemoryAttributionImmutableHierarchyTest,
       InfoMemoryAttributionPartiallyUnmappedBufferIsInvalidArgs) {
  ASSERT_NO_FATAL_FAILURE((CheckPartiallyUnmappedBufferIsError<zx_info_memory_attribution_t>(
      ZX_INFO_MEMORY_ATTRIBUTION, GetJobA, ZX_ERR_INVALID_ARGS)));
}

using MemoryAttributionMutableHierarchyTest = MemoryAttributionTest;

TEST_F(MemoryAttributionMutableHierarchyTest, FlatHierarchy) {
  std::set<zx_koid_t> visible_koids;
  zx::vmar scratch;

  zx::job job;
  ASSERT_OK(zx::job::create(*zx::job::default_job(), 0, &job));

  // Verify that the job is initially empty.
  ASSERT_NO_FATAL_FAILURE((EnumerateVisibleKoids(job, visible_koids)));
  EXPECT_TRUE(visible_koids.empty());

  // Add one process and verify that it's reported.
  zx::process proc_1;
  ASSERT_OK(zx::process::create(job, "p1", 2, 0, &proc_1, &scratch));
  ASSERT_NO_FATAL_FAILURE((EnumerateVisibleKoids(job, visible_koids)));
  EXPECT_TRUE(visible_koids == std::set<zx_koid_t>{GetKoid(proc_1)});

  // Add another process and verify that it's reported.
  zx::process proc_2;
  ASSERT_OK(zx::process::create(job, "p2", 2, 0, &proc_2, &scratch));
  ASSERT_NO_FATAL_FAILURE((EnumerateVisibleKoids(job, visible_koids)));
  EXPECT_TRUE((visible_koids == std::set<zx_koid_t>{GetKoid(proc_1), GetKoid(proc_2)}));

  // Drop the reference to the first process and verify that it's no longer reported.
  proc_1.reset();
  ASSERT_NO_FATAL_FAILURE((EnumerateVisibleKoids(job, visible_koids)));
  EXPECT_TRUE(visible_koids == std::set<zx_koid_t>{GetKoid(proc_2)});
}

TEST_F(MemoryAttributionMutableHierarchyTest, NestedHierarchy) {
  std::set<zx_koid_t> visible_koids;
  zx::vmar scratch;

  // Create the following nested hierarchy:
  //   JobA
  //     |--- Process1
  //     |--- JobB
  //            |--- Process2
  //            |--- JobC
  //                   |--- Process3
  zx::job job_a, job_b, job_c;
  zx::process proc_1, proc_2, proc_3;
  ASSERT_OK(zx::job::create(*zx::job::default_job(), 0, &job_a));
  ASSERT_OK(zx::job::create(job_a, 0, &job_b));
  ASSERT_OK(zx::job::create(job_b, 0, &job_c));
  ASSERT_OK(zx::process::create(job_a, "p1", 2, 0, &proc_1, &scratch));
  ASSERT_OK(zx::process::create(job_b, "p2", 2, 0, &proc_2, &scratch));
  ASSERT_OK(zx::process::create(job_c, "p3", 2, 0, &proc_3, &scratch));

  // Verify that each job can only see the processes below it.
  ASSERT_NO_FATAL_FAILURE((EnumerateVisibleKoids(job_a, visible_koids)));
  EXPECT_TRUE(
      (visible_koids == std::set<zx_koid_t>{GetKoid(proc_1), GetKoid(proc_2), GetKoid(proc_3)}));
  ASSERT_NO_FATAL_FAILURE((EnumerateVisibleKoids(job_b, visible_koids)));
  EXPECT_TRUE((visible_koids == std::set<zx_koid_t>{GetKoid(proc_2), GetKoid(proc_3)}));
  ASSERT_NO_FATAL_FAILURE((EnumerateVisibleKoids(job_c, visible_koids)));
  EXPECT_TRUE(visible_koids == std::set<zx_koid_t>{GetKoid(proc_3)});

  // Drop the reference to job B and verify that its descendants are still visible by job A.
  job_b.reset();
  ASSERT_NO_FATAL_FAILURE((EnumerateVisibleKoids(job_a, visible_koids)));
  EXPECT_TRUE(
      (visible_koids == std::set<zx_koid_t>{GetKoid(proc_1), GetKoid(proc_2), GetKoid(proc_3)}));
  ASSERT_NO_FATAL_FAILURE((EnumerateVisibleKoids(job_c, visible_koids)));
  EXPECT_TRUE(visible_koids == std::set<zx_koid_t>{GetKoid(proc_3)});

  // Add another job with another process under job A and verify visibility.
  zx::job job_d;
  zx::process proc_4;
  ASSERT_OK(zx::job::create(job_a, 0, &job_d));
  ASSERT_OK(zx::process::create(job_d, "p4", 2, 0, &proc_4, &scratch));
  ASSERT_NO_FATAL_FAILURE((EnumerateVisibleKoids(job_a, visible_koids)));
  EXPECT_TRUE((visible_koids == std::set<zx_koid_t>{GetKoid(proc_1), GetKoid(proc_2),
                                                    GetKoid(proc_3), GetKoid(proc_4)}));
  ASSERT_NO_FATAL_FAILURE((EnumerateVisibleKoids(job_c, visible_koids)));
  EXPECT_TRUE(visible_koids == std::set<zx_koid_t>{GetKoid(proc_3)});
  ASSERT_NO_FATAL_FAILURE((EnumerateVisibleKoids(job_d, visible_koids)));
  EXPECT_TRUE(visible_koids == std::set<zx_koid_t>{GetKoid(proc_4)});

  // Drop all the references to jobs except job A and verify that all the processes are still
  // visible.
  job_c.reset();
  job_d.reset();
  ASSERT_NO_FATAL_FAILURE((EnumerateVisibleKoids(job_a, visible_koids)));
  EXPECT_TRUE((visible_koids == std::set<zx_koid_t>{GetKoid(proc_1), GetKoid(proc_2),
                                                    GetKoid(proc_3), GetKoid(proc_4)}));
}

}  // namespace
}  // namespace object_info_test
