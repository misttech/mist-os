// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <gtest/gtest.h>

#include "src/developer/debug/ipc/filter_utils.h"
#include "src/developer/debug/ipc/records.h"

namespace debug_ipc {

TEST(FilterUtils, FilterMatches) {
  Filter filter{.type = debug_ipc::Filter::Type::kProcessName, .pattern = "foo"};
  EXPECT_TRUE(FilterMatches(filter, "foo", {}));
  EXPECT_FALSE(FilterMatches(filter, "foobar", {}));

  filter = {.type = debug_ipc::Filter::Type::kProcessNameSubstr, .pattern = "foo"};
  EXPECT_TRUE(FilterMatches(filter, "foo", {}));
  EXPECT_TRUE(FilterMatches(filter, "foobar", {}));

  filter = {.type = debug_ipc::Filter::Type::kComponentMoniker, .pattern = "/core/abc"};
  EXPECT_TRUE(FilterMatches(filter, "", {ComponentInfo{.moniker = "/core/abc"}}));
  EXPECT_FALSE(FilterMatches(filter, "", {ComponentInfo{.moniker = "/core/abc/def"}}));

  filter = {.type = debug_ipc::Filter::Type::kComponentMonikerSuffix, .pattern = "abc/def"};
  EXPECT_TRUE(FilterMatches(filter, "", {ComponentInfo{.moniker = "/core/abc/def"}}));
  EXPECT_FALSE(FilterMatches(filter, "", {ComponentInfo{.moniker = "/core/abc"}}));

  filter = {.type = debug_ipc::Filter::Type::kComponentName, .pattern = "foo.cm"};
  EXPECT_TRUE(FilterMatches(filter, "", {ComponentInfo{.url = "pkg://host#meta/foo.cm"}}));

  filter = {.type = debug_ipc::Filter::Type::kComponentUrl, .pattern = "pkg://host#meta/foo.cm"};
  EXPECT_TRUE(
      FilterMatches(filter, "", {ComponentInfo{.url = "pkg://host?hash=abcd#meta/foo.cm"}}));
}

TEST(FilterUtils, GetAttachConfigsForFilterMatches) {
  // None of the filters need patterns, because they've already been determined to be a match.
  Filter filter1 = {
      .id = 1,
      .config =
          {
              .weak = true,
          },
  };

  Filter filter2 = {
      .id = 2,
      .config = {},
  };

  Filter filter3 = {
      .id = 3,
      .config =
          {
              .job_only = true,
          },
  };

  std::vector<Filter> filters = {filter1, filter2, filter3};

  constexpr uint64_t kStrongProcessPid = 0x1236;
  constexpr uint64_t kJobPid = 0x1237;

  const std::vector<FilterMatch> kMatches = {
      FilterMatch(filter1.id, {0x1234, 0x1235, kStrongProcessPid}),
      FilterMatch(filter2.id, {kStrongProcessPid}),
      FilterMatch(filter3.id, {kJobPid}),
  };

  auto result = GetAttachConfigsForFilterMatches(kMatches, filters);

  // There are 4 unique pids.
  EXPECT_EQ(result.size(), 4u);

  // kStrongProcessPid is matched by both a strong a weak filter, the attach request should be
  // strong.
  auto strong_attach = result.find(kStrongProcessPid);
  ASSERT_NE(strong_attach, result.end());
  EXPECT_FALSE(strong_attach->second.weak);
  EXPECT_EQ(strong_attach->second.target, AttachConfig::Target::kProcess);

  auto job_attach = result.find(kJobPid);
  ASSERT_NE(job_attach, result.end());
  EXPECT_EQ(job_attach->second.target, AttachConfig::Target::kJob);
  EXPECT_FALSE(job_attach->second.weak);
}

}  // namespace debug_ipc
