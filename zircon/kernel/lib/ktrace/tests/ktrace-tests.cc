// Copyright 2025 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <align.h>
#include <lib/fxt/interned_category.h>
#include <lib/fxt/record_types.h>
#include <lib/fxt/serializer.h>
#include <lib/ktrace.h>
#include <lib/unittest/unittest.h>
#include <lib/zircon-internal/ktrace.h>

#include <arch/ops.h>

// A test version of the per-CPU KTrace instance that disables diagnostic logs and overrides
// ReportMetadata. We need to override ReportMetadata in tests because the base version emits trace
// records containing the names of all live threads and processes in the system to the global ktrace
// singleton's trace buffer, which we do not want to do in these unit tests.
class TestKTrace : public KTraceImpl<BufferMode::kPerCpu> {
 public:
  explicit TestKTrace() : KTraceImpl<BufferMode::kPerCpu>(true) {}
  void ReportMetadata() override { report_metadata_count_++; }

  uint32_t report_metadata_count() const { return report_metadata_count_; }

 private:
  uint32_t report_metadata_count_{0};
};

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

    TestKTrace ktrace;
    const uint32_t total_bufsize = PAGE_SIZE * arch_max_num_cpus();

    // Initialize the buffer with initial categories. Once complete:
    // * The per-CPU buffers should be allocated.
    // * The buffer_size_ and num_buffers_ should be set.
    // * Writes should be enabled.
    // * Categories should be set.
    // * Metadata should have been reported.
    ktrace.Init(total_bufsize, 0xff1u);
    ASSERT_NONNULL(ktrace.percpu_buffers_);
    ASSERT_EQ(static_cast<uint32_t>(PAGE_SIZE), ktrace.buffer_size_);
    ASSERT_EQ(arch_max_num_cpus(), ktrace.num_buffers_);
    ASSERT_TRUE(ktrace.WritesEnabled());
    ASSERT_EQ(0xff1u, ktrace.categories_bitmask());
    ASSERT_EQ(1u, ktrace.report_metadata_count());

    // Call Start and verify that:
    // * Writes remain enabled
    // * The categories change.
    // * Metadata was not reported a second time.
    ktrace.Control(KTRACE_ACTION_START, 0x203u);
    ASSERT_TRUE(ktrace.WritesEnabled());
    ASSERT_EQ(0x203u, ktrace.categories_bitmask());
    ASSERT_EQ(1u, ktrace.report_metadata_count());

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

    TestKTrace ktrace;
    const uint32_t total_bufsize = PAGE_SIZE * arch_max_num_cpus();

    // Initialize the buffer with no initial categories. Once complete:
    // * No per-CPU buffers should be allocated.
    // * The buffer_size_ and num_buffers_ should be set.
    // * Writes should be disabled.
    // * Categories should be set to zero.
    // * Metadata should _not_ have been reported.
    ktrace.Init(total_bufsize, 0u);
    ASSERT_NULL(ktrace.percpu_buffers_);
    ASSERT_EQ(static_cast<uint32_t>(PAGE_SIZE), ktrace.buffer_size_);
    ASSERT_EQ(arch_max_num_cpus(), ktrace.num_buffers_);
    ASSERT_FALSE(ktrace.WritesEnabled());
    ASSERT_EQ(0u, ktrace.categories_bitmask());
    ASSERT_EQ(0u, ktrace.report_metadata_count());

    // Start tracing and verify that:
    // * The per-CPU buffers have been allocated.
    // * Writes have been enabled.
    // * Categories have been set.
    // * Metadata was reported.
    ktrace.Control(KTRACE_ACTION_START, 0x1fu);
    ASSERT_NONNULL(ktrace.percpu_buffers_);
    ASSERT_TRUE(ktrace.WritesEnabled());
    ASSERT_EQ(0x1fu, ktrace.categories_bitmask());
    ASSERT_EQ(1u, ktrace.report_metadata_count());

    // Call Start again and verify that:
    // * Writes remain enabled.
    // * The categories change.
    // * Metadata was not reported a second time.
    ktrace.Control(KTRACE_ACTION_START, 0x20u);
    ASSERT_TRUE(ktrace.WritesEnabled());
    ASSERT_EQ(0x20u, ktrace.categories_bitmask());
    ASSERT_EQ(1u, ktrace.report_metadata_count());

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

  // Test that writes work as expected.
  static bool TestWrite() {
    BEGIN_TEST;

    // NOTE: The SpscBuffer tests already verify that writes to a single buffer work as expected,
    // so we do not duplicate those tests here. Instead, this test verifies KTrace specific write
    // behaviors, such as:
    // * Reserve should fail if writes are disabled.
    // * Reserve should always pick the per-CPU buffer associated with the current CPU to write to.
    // * Reserve should correctly parse the required slot size from an FXT header.
    // * PendingCommit should be able to write a single word using WriteWord correctly.
    // * PendingCommit should be able to write a buffer of bytes using WriteBytes correctly, and it
    //   should correctly pad to the nearest word.

    // Generate data that we can write into our KTrace buffer. Intentionally generate a non-word
    // aligned amount to test the padding behavior.
    //
    // Words to write with PendingCommit::WriteWord.
    ktl::array<uint64_t, 29> words;
    // Bytes to write with PendingCommit::WriteBytes.
    ktl::array<ktl::byte, 397> bytes;
    static_assert(bytes.size() % 8 != 0);
    constexpr size_t unpadded_record_size = sizeof(uint64_t) + (words.size() * 8) + bytes.size();
    constexpr uint64_t padded_record_size = ALIGN(unpadded_record_size, 8);
    constexpr uint64_t fxt_header =
        fxt::MakeHeader(fxt::RecordType::kBlob, fxt::WordSize::FromBytes(unpadded_record_size));

    // Populate the trace record with random data.
    srand(4);
    for (uint64_t& word : words) {
      word = static_cast<uint64_t>(rand());
    }
    for (ktl::byte& byte : bytes) {
      byte = static_cast<ktl::byte>(rand());
    }

    // Initialize KTrace, but do not start tracing.
    TestKTrace ktrace;
    const uint32_t total_bufsize = PAGE_SIZE * arch_max_num_cpus();
    ktrace.Init(total_bufsize, 0u);

    // Verify that attempting to Reserve a slot now fails because tracing has not been started, and
    // therefore writes are disabled.
    zx::result<TestKTrace::Reservation> failed = ktrace.Reserve(fxt_header);
    ASSERT_EQ(ZX_ERR_BAD_STATE, failed.status_value());

    // Start tracing.
    ASSERT_OK(ktrace.Control(KTRACE_ACTION_START, 0xfff));

    // Successfully reserve a slot.
    zx::result<TestKTrace::Reservation> res = ktrace.Reserve(fxt_header);
    ASSERT_OK(res.status_value());

    // Reserve turns off interrupts, so we can get the number of the CPU whose buffer we're writing
    // to.
    const cpu_num_t target_cpu = arch_curr_cpu_num();

    // Write the trace record.
    for (uint64_t word : words) {
      res->WriteWord(word);
    }
    res->WriteBytes(bytes.data(), bytes.size());
    res->Commit();

    // Read out the data.
    uint8_t actual[padded_record_size];
    auto copy_out = [&](uint32_t offset, ktl::span<ktl::byte> src) {
      memcpy(actual + offset, src.data(), src.size());
      return ZX_OK;
    };
    zx::result<size_t> read_result =
        ktrace.percpu_buffers_[target_cpu].Read(copy_out, padded_record_size);
    ASSERT_OK(read_result.status_value());
    ASSERT_EQ(padded_record_size, read_result.value());

    // Verify that the data is what we expect.
    uint8_t expected[padded_record_size]{};
    memcpy(expected, &fxt_header, sizeof(fxt_header));
    memcpy(expected + sizeof(fxt_header), words.data(), words.size() * 8);
    memcpy(expected + sizeof(fxt_header) + (words.size() * 8), bytes.data(), bytes.size());
    ASSERT_BYTES_EQ(expected, actual, padded_record_size);

    END_TEST;
  }
};

UNITTEST_START_TESTCASE(ktrace_tests)
UNITTEST("legacy_categories", KTraceTests::TestLegacyCategories)
UNITTEST("init_stop", KTraceTests::TestInitStop)
UNITTEST("start_stop", KTraceTests::TestStartStop)
UNITTEST("write", KTraceTests::TestWrite)
UNITTEST_END_TESTCASE(ktrace_tests, "ktrace", "KTrace tests")
