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

  // Verify that categories are enabled and disabled correctly when using single buffer mode.
  static bool TestLegacyCategories() {
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

  // Test the case where tracing is started by Init and stopped by Stop.
  static bool TestInitStop() {
    BEGIN_TEST;

    KTraceImpl<BufferMode::kPerCpu> ktrace(true);
    const uint32_t total_bufsize = PAGE_SIZE * arch_max_num_cpus();

    // Initialize the buffer with initial categories. Once complete:
    // * The per-CPU buffers should be allocated.
    // * The buffer_size_ and num_buffers_ should be set.
    // * Writes should be enabled.
    // * Categories should be set.
    // * TODO(rudymathu): Verify that metadata was reported once Writes are implemented.
    ktrace.Init(total_bufsize, 0xff1u);
    ASSERT_NONNULL(ktrace.percpu_buffers_);
    ASSERT_EQ(static_cast<uint32_t>(PAGE_SIZE), ktrace.buffer_size_);
    ASSERT_EQ(arch_max_num_cpus(), ktrace.num_buffers_);
    ASSERT_TRUE(ktrace.WritesEnabled());
    ASSERT_EQ(0xff1u, ktrace.categories_bitmask());

    // Call Start and verify that:
    // * Writes remain enabled
    // * The categories change.
    // * TODO(rudymathu): Verify that metadata was not reported once Writes are implemented.
    ktrace.Control(KTRACE_ACTION_START, 0x203u);
    ASSERT_TRUE(ktrace.WritesEnabled());
    ASSERT_EQ(0x203u, ktrace.categories_bitmask());

    // Call Stop and verify that:
    // * The percpu_buffers_ remain allocated.
    // * Writes are disabled.
    // * The categories bitmask is cleared.
    ktrace.Control(KTRACE_ACTION_STOP, 0u);
    ASSERT_NONNULL(ktrace.percpu_buffers_);
    ASSERT_FALSE(ktrace.WritesEnabled());
    ASSERT_EQ(0u, ktrace.categories_bitmask());

    END_TEST;
  }

  // Test the case where tracing is started by Start and stopped by Stop.
  static bool TestStartStop() {
    BEGIN_TEST;

    KTraceImpl<BufferMode::kPerCpu> ktrace(true);
    const uint32_t total_bufsize = PAGE_SIZE * arch_max_num_cpus();

    // Initialize the buffer with no initial categories. Once complete:
    // * No per-CPU buffers should be allocated.
    // * The buffer_size_ and num_buffers_ should be set.
    // * Writes should be disabled.
    // * Categories should be set to zero.
    ktrace.Init(total_bufsize, 0u);
    ASSERT_NULL(ktrace.percpu_buffers_);
    ASSERT_EQ(static_cast<uint32_t>(PAGE_SIZE), ktrace.buffer_size_);
    ASSERT_EQ(arch_max_num_cpus(), ktrace.num_buffers_);
    ASSERT_FALSE(ktrace.WritesEnabled());
    ASSERT_EQ(0u, ktrace.categories_bitmask());

    // Start tracing and verify that:
    // * The per-CPU buffers have been allocated.
    // * TODO(rudymathu): Verify that metadata was reported once Writes are implemented.
    // * Writes have been enabled.
    // * Categories have been set.
    ktrace.Control(KTRACE_ACTION_START, 0x1fu);
    ASSERT_NONNULL(ktrace.percpu_buffers_);
    ASSERT_TRUE(ktrace.WritesEnabled());
    ASSERT_EQ(0x1fu, ktrace.categories_bitmask());

    // Call Start again and verify that:
    // * Writes remain enabled.
    // * The categories change.
    // * TODO(rudymathu): Verify that metadata was not reported once Writes are implemented.
    ktrace.Control(KTRACE_ACTION_START, 0x20u);
    ASSERT_TRUE(ktrace.WritesEnabled());
    ASSERT_EQ(0x20u, ktrace.categories_bitmask());

    // Stop tracing and verify that:
    // * The percpu_buffers_ remain allocated.
    // * Writes are disabled.
    // * The categories bitmask is cleared.
    ktrace.Control(KTRACE_ACTION_STOP, 0u);
    ASSERT_NONNULL(ktrace.percpu_buffers_);
    ASSERT_FALSE(ktrace.WritesEnabled());
    ASSERT_EQ(0u, ktrace.categories_bitmask());

    END_TEST;
  }
};

UNITTEST_START_TESTCASE(ktrace_tests)
UNITTEST("legacy_categories", KTraceTests::TestLegacyCategories)
UNITTEST("init_stop", KTraceTests::TestInitStop)
UNITTEST("start_stop", KTraceTests::TestStartStop)
UNITTEST_END_TESTCASE(ktrace_tests, "ktrace", "KTrace tests")
