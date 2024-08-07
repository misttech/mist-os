// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/fit/defer.h>
#include <zircon/assert.h>

#ifdef _KERNEL_MISTOS
#include <ktl/algorithm.h>
#else
#include <algorithm>
#endif
#include <cstdlib>
#include <memory>
#include <utility>

#include <fbl/alloc_checker.h>
#include <zxtest/base/test-case.h>
#include <zxtest/base/types.h>

namespace zxtest {

namespace {

using internal::SetUpTestCaseFn;
using internal::TearDownTestCaseFn;
using internal::TestDriver;
using internal::TestStatus;

}  // namespace

TestCase::TestCase(const fbl::String& name, SetUpTestCaseFn set_up, TearDownTestCaseFn tear_down)
    : name_(name), set_up_(std::move(set_up)), tear_down_(std::move(tear_down)) {
  ZX_ASSERT_MSG(set_up_, "Invalid SetUpTestCaseFn");
  ZX_ASSERT_MSG(tear_down_, "Invalid TearDownTestCaseFn");
}
TestCase::TestCase(TestCase&& other) = default;
TestCase::~TestCase() = default;

size_t TestCase::TestCount() const { return test_infos_.size(); }

size_t TestCase::MatchingTestCount() const { return selected_indexes_.size(); }

void TestCase::Filter(TestCase::FilterFn filter) {
  fbl::Vector<unsigned long> filtered_indexes;
  fbl::AllocChecker ac;
  filtered_indexes.reserve(test_infos_.size(), &ac);
  ZX_ASSERT(ac.check());
  for (unsigned long i = 0; i < test_infos_.size(); ++i) {
    const auto& test_info = test_infos_[i];
    if (!filter || filter(name_, test_info.name())) {
      filtered_indexes.push_back(i, &ac);
      ZX_ASSERT(ac.check());
    }
  }
  selected_indexes_.swap(filtered_indexes);
}

void TestCase::Shuffle(uint32_t random_seed) {
  for (unsigned long i = 1; i < selected_indexes_.size(); ++i) {
#ifdef _KERNEL_MISTOS
    uintptr_t seed = random_seed;
    unsigned long j = rand_r(&seed) % (i + 1);
#else
    unsigned long j = rand_r(&random_seed) % (i + 1);
#endif
    if (j != i) {
      std::swap(selected_indexes_[i], selected_indexes_[j]);
    }
  }
}

void TestCase::UnShuffle() {
  // Put the, possibly filtered, list back in order.
#ifdef _KERNEL_MISTOS
  ktl::stable_sort(selected_indexes_.begin(), selected_indexes_.end());
#else
  std::sort(selected_indexes_.begin(), selected_indexes_.end());
#endif
}

bool TestCase::RegisterTest(const fbl::String& name, const SourceLocation& location,
                            internal::TestFactory factory) {
  auto it = std::find_if(test_infos_.begin(), test_infos_.end(),
                         [&name](const TestInfo& info) { return info.name() == name; });

  // Test already registered.
  if (it != test_infos_.end()) {
    return false;
  }

  fbl::AllocChecker ac;
  selected_indexes_.push_back(selected_indexes_.size(), &ac);
  ZX_ASSERT(ac.check());
  test_infos_.push_back(TestInfo(name, location, std::move(factory)), &ac);
  ZX_ASSERT(ac.check());
  return true;
}

void TestCase::Run(LifecycleObserver* event_broadcaster, TestDriver* driver) {
  if (selected_indexes_.size() == 0) {
    return;
  }

  auto tear_down = fit::defer([this, event_broadcaster] {
    tear_down_();
    event_broadcaster->OnTestCaseEnd(*this);
  });
  event_broadcaster->OnTestCaseStart(*this);
  set_up_();

  if (!driver->Continue()) {
    return;
  }

  for (unsigned long i = 0; i < selected_indexes_.size(); ++i) {
    const auto& test_info = test_infos_[selected_indexes_[i]];
    {
      // This block enforces that the destructor is called before
      // completing the test, for accurate error reporting.
      // This prevents buggy destructors from crashing after a test completed,
      // marking it as a success.
      event_broadcaster->OnTestStart(*this, test_info);
      std::unique_ptr<Test> test = test_info.Instantiate(driver);
      test->Run();
    }
    switch (driver->Status()) {
      case TestStatus::kPassed:
        event_broadcaster->OnTestSuccess(*this, test_info);
        break;
      case TestStatus::kSkipped:
        event_broadcaster->OnTestSkip(*this, test_info);
        break;
      case TestStatus::kFailed:
        event_broadcaster->OnTestFailure(*this, test_info);
        if (return_on_failure_) {
          return;
        }
        break;
      default:
        break;
    }
  }
}

}  // namespace zxtest
