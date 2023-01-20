// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/forensics/feedback/annotations/intl_provider.h"

#include <lib/syslog/cpp/macros.h>
#include <lib/zx/time.h>
#include <zircon/errors.h>

#include <map>
#include <memory>
#include <string>

#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "src/developer/forensics/feedback/annotations/constants.h"
#include "src/developer/forensics/testing/stubs/intl_provider.h"
#include "src/developer/forensics/testing/unit_test_fixture.h"
#include "src/developer/forensics/utils/errors.h"
#include "src/lib/backoff/backoff.h"

namespace forensics::feedback {
namespace {

using ::testing::ElementsAreArray;
using ::testing::IsEmpty;
using ::testing::Pair;
using ::testing::UnorderedElementsAreArray;

class MonotonicBackoff : public backoff::Backoff {
 public:
  zx::duration GetNext() override {
    const auto backoff = backoff_;
    backoff_ = backoff + zx::sec(1);
    return backoff;
  }
  void Reset() override { backoff_ = zx::sec(1); }

 private:
  zx::duration backoff_{zx::sec(1)};
};

using IntlProviderTest = UnitTestFixture;

TEST_F(IntlProviderTest, GetKeys) {
  IntlProvider provider(dispatcher(), services(), std::make_unique<MonotonicBackoff>());
  EXPECT_THAT(provider.GetKeys(), UnorderedElementsAreArray({kSystemTimezonePrimaryKey}));
}

TEST_F(IntlProviderTest, GetOnUpdate) {
  stubs::IntlProvider server("timezone-one");
  InjectServiceProvider(&server);

  IntlProvider provider(dispatcher(), services(), std::make_unique<MonotonicBackoff>());
  Annotations annotations;

  provider.GetOnUpdate([&annotations](Annotations result) { annotations = std::move(result); });

  EXPECT_THAT(annotations, IsEmpty());

  RunLoopUntilIdle();
  EXPECT_THAT(annotations, UnorderedElementsAreArray({
                               Pair(kSystemTimezonePrimaryKey, "timezone-one"),
                           }));

  server.SetTimezone("timezone-two");

  // The change hasn't propagated yet.
  EXPECT_THAT(annotations, UnorderedElementsAreArray({
                               Pair(kSystemTimezonePrimaryKey, "timezone-one"),
                           }));

  RunLoopUntilIdle();
  EXPECT_THAT(annotations, UnorderedElementsAreArray({
                               Pair(kSystemTimezonePrimaryKey, "timezone-two"),
                           }));
}

TEST_F(IntlProviderTest, Reconnects) {
  stubs::IntlProvider server("timezone-one");
  InjectServiceProvider(&server);

  IntlProvider provider(dispatcher(), services(), std::make_unique<MonotonicBackoff>());
  Annotations annotations;

  provider.GetOnUpdate([&annotations](Annotations result) { annotations = std::move(result); });

  EXPECT_THAT(annotations, IsEmpty());

  RunLoopUntilIdle();
  EXPECT_THAT(annotations, UnorderedElementsAreArray({
                               Pair(kSystemTimezonePrimaryKey, "timezone-one"),
                           }));

  server.CloseConnection();
  ASSERT_FALSE(server.IsBound());

  server.SetTimezone("timezone-two");

  // The previously cached value should be used.
  RunLoopUntilIdle();
  EXPECT_THAT(annotations, UnorderedElementsAreArray({
                               Pair(kSystemTimezonePrimaryKey, "timezone-one"),
                           }));
  RunLoopFor(zx::sec(1));
  ASSERT_TRUE(server.IsBound());
  EXPECT_THAT(annotations, UnorderedElementsAreArray({
                               Pair(kSystemTimezonePrimaryKey, "timezone-two"),
                           }));
}

}  // namespace
}  // namespace forensics::feedback
