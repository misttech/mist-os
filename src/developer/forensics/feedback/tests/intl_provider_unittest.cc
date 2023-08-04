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
  EXPECT_THAT(provider.GetKeys(), UnorderedElementsAreArray({
                                      kSystemLocalePrimaryKey,
                                      kSystemTimezonePrimaryKey,
                                  }));
}

TEST_F(IntlProviderTest, GetOnUpdateLocale) {
  stubs::IntlProvider server("locale-one", /*default_timezone=*/std::nullopt);
  InjectServiceProvider(&server);

  IntlProvider provider(dispatcher(), services(), std::make_unique<MonotonicBackoff>());
  Annotations annotations;

  provider.GetOnUpdate([&annotations](Annotations result) { annotations = std::move(result); });

  EXPECT_THAT(annotations, IsEmpty());

  RunLoopUntilIdle();
  EXPECT_THAT(annotations, UnorderedElementsAreArray({
                               Pair(kSystemLocalePrimaryKey, "locale-one"),
                           }));

  server.SetLocale("locale-two");

  // The change hasn't propagated yet.
  EXPECT_THAT(annotations, UnorderedElementsAreArray({
                               Pair(kSystemLocalePrimaryKey, "locale-one"),
                           }));

  RunLoopUntilIdle();
  EXPECT_THAT(annotations, UnorderedElementsAreArray({
                               Pair(kSystemLocalePrimaryKey, "locale-two"),
                           }));
}

TEST_F(IntlProviderTest, GetOnUpdateTimezone) {
  stubs::IntlProvider server(/*default_locale=*/std::nullopt, "timezone-one");
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

TEST_F(IntlProviderTest, GetOnUpdate) {
  stubs::IntlProvider server("locale-one", "timezone-one");
  InjectServiceProvider(&server);

  IntlProvider provider(dispatcher(), services(), std::make_unique<MonotonicBackoff>());
  Annotations annotations;

  provider.GetOnUpdate([&annotations](Annotations result) { annotations = std::move(result); });

  EXPECT_THAT(annotations, IsEmpty());

  RunLoopUntilIdle();
  EXPECT_THAT(annotations, UnorderedElementsAreArray({
                               Pair(kSystemLocalePrimaryKey, "locale-one"),
                               Pair(kSystemTimezonePrimaryKey, "timezone-one"),
                           }));

  server.SetLocale("locale-two");
  RunLoopUntilIdle();

  EXPECT_THAT(annotations, UnorderedElementsAreArray({
                               Pair(kSystemLocalePrimaryKey, "locale-two"),
                               Pair(kSystemTimezonePrimaryKey, "timezone-one"),
                           }));

  server.SetTimezone("timezone-two");
  RunLoopUntilIdle();

  EXPECT_THAT(annotations, UnorderedElementsAreArray({
                               Pair(kSystemLocalePrimaryKey, "locale-two"),
                               Pair(kSystemTimezonePrimaryKey, "timezone-two"),
                           }));
}

TEST_F(IntlProviderTest, Reconnects) {
  stubs::IntlProvider server(/*default_locale=*/std::nullopt, "timezone-one");
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

TEST_F(IntlProviderTest, DoesNotReconnectIfUnavailable) {
  stubs::IntlProvider server(/*default_locale=*/std::nullopt, "timezone-one");
  InjectServiceProvider(&server);

  IntlProvider provider(dispatcher(), services(), std::make_unique<MonotonicBackoff>());
  Annotations annotations;

  provider.GetOnUpdate([&annotations](Annotations result) { annotations = std::move(result); });
  EXPECT_THAT(annotations, IsEmpty());

  RunLoopUntilIdle();
  ASSERT_TRUE(server.IsBound());

  server.CloseConnection(ZX_ERR_UNAVAILABLE);

  RunLoopUntilIdle();
  EXPECT_FALSE(server.IsBound());

  // Run past monotonic backoff.
  RunLoopFor(zx::sec(2));
  EXPECT_FALSE(server.IsBound());

  EXPECT_THAT(annotations, UnorderedElementsAreArray({
                               Pair(kSystemLocalePrimaryKey, Error::kNotAvailableInProduct),
                               Pair(kSystemTimezonePrimaryKey, Error::kNotAvailableInProduct),
                           }));
}

}  // namespace
}  // namespace forensics::feedback
