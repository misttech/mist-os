// Copyright 2025 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/fxt/interned_category.h>
#include <lib/ktrace.h>
#include <lib/unittest/unittest.h>
#include <lib/zircon-internal/ktrace.h>

// The KTraceTests class is a friend of the KTrace class, which allows it to access private members
// of that class.
class KTraceTests {
 public:
  static constexpr uint32_t kDefaultBufferSize = 4096;

  // Verify that categories are enabled and disabled correctly.
  static bool TestCategories() {
    BEGIN_TEST;

    KTrace ktrace(true);
    ktrace.internal_state_.disable_diags_printfs_ = true;

    // Test that no categories are enabled by default.
    ASSERT_EQ(0u, ktrace.categories_bitmask());

    // Verify that Init correctly sets the categories bitmask.
    const uint32_t kInitCategories = KTRACE_GRP_SCHEDULER | KTRACE_GRP_SYSCALL_BIT;
    ktrace.Init(kDefaultBufferSize, kInitCategories);
    ASSERT_EQ(kInitCategories, ktrace.categories_bitmask());

    // Verify that Start changes the categories bitmask.
    const uint32_t kStartCategories = KTRACE_GRP_IPC | KTRACE_GRP_ARCH;
    ASSERT_OK(ktrace.Control(KTRACE_ACTION_START, kStartCategories));
    ASSERT_EQ(kStartCategories, ktrace.categories_bitmask());

    // Verify that Stop resets the categories bitmask to zero.
    ASSERT_OK(ktrace.Control(KTRACE_ACTION_STOP, 0));
    ASSERT_EQ(0u, ktrace.categories_bitmask());

    END_TEST;
  }
};

UNITTEST_START_TESTCASE(ktrace_tests)
UNITTEST("categories", KTraceTests::TestCategories)
UNITTEST_END_TESTCASE(ktrace_tests, "ktrace", "KTrace tests")
