// Copyright 2021 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/fit/defer.h>
#include <lib/fxt/serializer.h>
#include <lib/ktrace/ktrace_internal.h>
#include <lib/unittest/unittest.h>

#include <ktl/algorithm.h>
#include <ktl/limits.h>
#include <ktl/unique_ptr.h>
#include <vm/vm_aspace.h>

#include <ktl/enforce.h>

namespace ktrace_tests {

// A test version of KTraceState which overrides the ReportStaticNames and
// ReportThreadProcessNames behaviors for testing purposes.
class TestKTraceState : public ::internal::KTraceState {
 public:
  using StartMode = internal::KTraceState::StartMode;
  static constexpr uint32_t kDefaultBufferSize = 4096;

  // Immediately after initialization, ktrace will write two metadata records
  // expressing the version of the trace buffer format, as well as the
  // resolution of the timestamps in the trace.  This initial offset value
  // reflects that.
  static constexpr uint32_t kInitialOffset = sizeof(uint64_t) * 3;

  // Figure out how many 32 byte records we should be able to fit into our
  // default buffer size, minus the two metadata records we consume up front.  Since we
  // always allocate buffers in multiples of page size, we should be able to
  // assert that this is an integral number of records.
  static_assert(kDefaultBufferSize > kInitialOffset);
  static_assert(((kDefaultBufferSize - kInitialOffset) % 8) == 0);
  static constexpr uint32_t kMaxWords = (kDefaultBufferSize - kInitialOffset) / sizeof(uint64_t);

  static bool InitStartTest() {
    BEGIN_TEST;

    constexpr uint32_t kAllGroups = KTRACE_GRP_ALL;

    {
      // Construct a ktrace state and initialize it, providing no group mask.
      // No buffer should be allocated, and no calls should be made to any of
      // the report hooks.  The only thing which should stick during this
      // operation is our target bufsize.
      TestKTraceState state;
      ASSERT_TRUE(state.Init(kDefaultBufferSize, 0));
      {
        Guard<TraceDisabledSpinLock, IrqSave> guard{&state.write_lock_};
        EXPECT_NULL(state.buffer_);
        EXPECT_EQ(0u, state.bufsize_);
        EXPECT_EQ(kDefaultBufferSize, state.target_bufsize_);
        EXPECT_EQ(0u, state.static_name_report_count_);
        EXPECT_EQ(0u, state.thread_name_report_count_);
        EXPECT_EQ(0u, state.grpmask());
      }

      // Attempting to start with no groups specified is not allowed.  We should
      // get "INVALID_ARGS" back.
      ASSERT_EQ(ZX_ERR_INVALID_ARGS, state.Start(0, StartMode::Saturate));

      // Now go ahead and call start.  This should cause the buffer to become
      // allocated, and for both static and thread names to be reported (static
      // before thread)
      ASSERT_OK(state.Start(kAllGroups, StartMode::Saturate));
      {
        Guard<TraceDisabledSpinLock, IrqSave> guard{&state.write_lock_};
        EXPECT_NONNULL(state.buffer_);
        EXPECT_GT(state.bufsize_, 0u);
        EXPECT_LE(state.bufsize_, state.target_bufsize_);
        EXPECT_EQ(kDefaultBufferSize, state.target_bufsize_);
        EXPECT_EQ(1u, state.static_name_report_count_);
        EXPECT_EQ(1u, state.thread_name_report_count_);
        EXPECT_LE(state.last_static_name_report_time_, state.last_thread_name_report_time_);
        EXPECT_EQ(kAllGroups, state.grpmask());
      }
    }

    {
      // Perform a similar test, but this time passing a non-zero group mask to
      // init.  This should cause tracing to go live immediately.
      TestKTraceState state;
      ASSERT_TRUE(state.Init(kDefaultBufferSize, kAllGroups));

      {
        Guard<TraceDisabledSpinLock, IrqSave> guard{&state.write_lock_};
        EXPECT_NONNULL(state.buffer_);
        EXPECT_GT(state.bufsize_, 0u);
        EXPECT_LE(state.bufsize_, state.target_bufsize_);
        EXPECT_EQ(kDefaultBufferSize, state.target_bufsize_);
        EXPECT_EQ(1u, state.static_name_report_count_);
        EXPECT_EQ(1u, state.thread_name_report_count_);
        EXPECT_LE(state.last_static_name_report_time_, state.last_thread_name_report_time_);
        EXPECT_EQ(kAllGroups, state.grpmask());
      }
    }

    {
      // Initialize a trace, then start it in circular mode.
      TestKTraceState state;
      ASSERT_TRUE(state.Init(kDefaultBufferSize, 0));
      ASSERT_OK(state.Start(kAllGroups, StartMode::Circular));

      // Stopping and starting the trace again in circular mode should be OK.
      ASSERT_OK(state.Stop());
      ASSERT_OK(state.Start(kAllGroups, StartMode::Circular));

      // Stopping and starting the trace again in saturate mode should be an
      // error.
      ASSERT_OK(state.Stop());
      ASSERT_EQ(ZX_ERR_BAD_STATE, state.Start(kAllGroups, StartMode::Saturate));

      // Rewinding the buffer should fix the issue.
      // error.
      ASSERT_OK(state.Rewind());
      ASSERT_OK(state.Start(kAllGroups, StartMode::Saturate));
    }

    END_TEST;
  }

  static bool WriteRecordsTest() {
    BEGIN_TEST;

    // Create a small trace buffer and initialize it.
    constexpr uint32_t kGroups = 0x3;
    TestKTraceState state;
    ASSERT_TRUE(state.Init(kDefaultBufferSize, kGroups));

    ASSERT_TRUE(state.CheckExpectedOffset(kInitialOffset));
    fxt::WriteInstantEventRecord(&state, 0xAAAA'AAAA'AAAA'AAAA, fxt::ThreadRef{0x0A},
                                 fxt::StringRef{1}, fxt::StringRef{2});
    fxt::WriteInstantEventRecord(&state, 1, fxt::ThreadRef{0x0B}, fxt::StringRef{3},
                                 fxt::StringRef{0x4});

    // Now, stop the trace and read the records back out and verify their contents.
    uint32_t records_enumerated = 0;
    auto checker = [&](const uint64_t* record) -> bool {
      BEGIN_TEST;
      const uint16_t threadref = fxt::EventRecordFields::ThreadRef::Get<uint16_t>(*record);
      switch (threadref) {
        case 0xAA:
          EXPECT_EQ(uint64_t{0x0001'0002'0A00'0024}, record[0]);
          EXPECT_EQ(uint64_t{0x0}, record[1]);
          break;
        case 0xBB:
          EXPECT_EQ(uint64_t{0x0003'0004'0B00'0024}, record[0]);
          EXPECT_EQ(uint64_t{0x1}, record[1]);
          break;
      }
      END_TEST;
    };

    ASSERT_OK(state.Stop());
    ASSERT_TRUE(state.TestAllRecords(records_enumerated, checker));
    EXPECT_EQ(2u, records_enumerated);

    END_TEST;
  }

  static bool SaturationTest() {
    BEGIN_TEST;

    // Create a small trace buffer and initialize it.
    constexpr uint32_t kGroups = KTRACE_GRP_PROBE;
    TestKTraceState state;
    ASSERT_TRUE(state.Init(kDefaultBufferSize, kGroups));

    // Write the (max - 1) 32 byte records to the buffer, then write a single 24 byte record.
    for (uint32_t i = 0; i + 2 < kMaxWords; i += 2) {
      // Instant records are 2 words
      fxt::WriteInstantEventRecord(&state, 0, fxt::ThreadRef{0x0A}, fxt::StringRef{0x1},
                                   fxt::StringRef{0x2});
    }

    // The buffer will not think that it is full just yet.
    uint32_t rcnt = 0;
    auto checker = [&](const uint64_t* hdr) -> bool { return true; };
    EXPECT_EQ(kGroups, state.grpmask());
    ASSERT_OK(state.Stop());
    EXPECT_TRUE(state.TestAllRecords(rcnt, checker));
    EXPECT_TRUE(state.CheckExpectedOffset(kDefaultBufferSize - 8));
    EXPECT_EQ(kMaxWords / 2, rcnt);

    // Now write one more record, this time with a different payload.
    ASSERT_OK(state.Start(kGroups, internal::KTraceState::StartMode::Saturate));
    fxt::WriteInstantEventRecord(&state, 0xABCDABCD, fxt::ThreadRef{0x0B}, fxt::StringRef{0x3},
                                 fxt::StringRef{0x4});

    // The buffer should now think that it is full (the group mask will be
    // cleared), and all of the original records should be present (but not the
    // new one).
    EXPECT_EQ(0u, state.grpmask());
    ASSERT_OK(state.Stop());

    auto payload_checker = [&](const uint64_t* hdr) -> bool {
      BEGIN_TEST;
      ASSERT_NONNULL(hdr);
      const size_t len = fxt::EventRecordFields::RecordSize::Get<size_t>(*hdr);
      const uint64_t* ts = reinterpret_cast<const uint64_t*>(hdr + 1);
      // All records that we get should be the first record
      EXPECT_EQ(uint64_t{0x0002'0001'0A00'0024}, *hdr);
      EXPECT_EQ(uint64_t{0}, *ts);
      EXPECT_EQ(2u, len);
      END_TEST;
    };
    EXPECT_TRUE(state.TestAllRecords(rcnt, payload_checker));
    EXPECT_EQ(kMaxWords / 2, rcnt);

    END_TEST;
  }

  static bool RewindTest() {
    BEGIN_TEST;

    // Create a small trace buffer and initialize it.
    constexpr uint32_t kGroups = KTRACE_GRP_PROBE;
    TestKTraceState state;
    ASSERT_TRUE(state.Init(kDefaultBufferSize, kGroups));

    ASSERT_TRUE(state.CheckExpectedOffset(kInitialOffset));

    // Write a couple of records.
    fxt::WriteInstantEventRecord(&state, 0, fxt::ThreadRef{0x0A}, fxt::StringRef{0x1},
                                 fxt::StringRef{0x2});

    fxt::WriteInstantEventRecord(&state, 1, fxt::ThreadRef{0x0A}, fxt::StringRef{0x1},
                                 fxt::StringRef{0x2});

    // The offset should have moved, and the number of records in the buffer should now be 2.
    uint32_t rcnt = 0;
    auto checker = [&](const uint64_t* hdr) -> bool { return true; };
    EXPECT_TRUE(state.CheckExpectedOffset(kInitialOffset, CheckOp::LT));
    EXPECT_EQ(kGroups, state.grpmask());
    ASSERT_OK(state.Stop());
    EXPECT_TRUE(state.TestAllRecords(rcnt, checker));
    EXPECT_EQ(2u, rcnt);

    // Rewind.  The offset should return to the beginning, and there
    // should be no records in the buffer.
    ASSERT_OK(state.Rewind());
    EXPECT_TRUE(state.CheckExpectedOffset(kInitialOffset));
    EXPECT_EQ(0u, state.grpmask());
    EXPECT_TRUE(state.TestAllRecords(rcnt, checker));
    EXPECT_EQ(0u, rcnt);

    // Start again, and this time saturate the buffer
    ASSERT_OK(state.Start(kGroups, StartMode::Saturate));
    for (uint32_t i = 0; i <= kMaxWords; i += 2) {
      fxt::WriteInstantEventRecord(&state, 0, fxt::ThreadRef{0x0A}, fxt::StringRef{0x1},
                                   fxt::StringRef{0x2});
    }
    EXPECT_EQ(0u, state.grpmask());
    ASSERT_OK(state.Stop());
    EXPECT_TRUE(state.TestAllRecords(rcnt, checker));
    EXPECT_EQ(kMaxWords / 2, rcnt);

    // Finally, rewind again.  The offset should return to the
    // beginning, and there should be no records in the buffer.
    ASSERT_OK(state.Rewind());
    EXPECT_TRUE(state.CheckExpectedOffset(kInitialOffset));
    EXPECT_EQ(0u, state.grpmask());
    EXPECT_TRUE(state.TestAllRecords(rcnt, checker));
    EXPECT_EQ(0u, rcnt);

    END_TEST;
  }

  static bool StateCheckTest() {
    BEGIN_TEST;

    constexpr uint32_t kAllGroups = KTRACE_GRP_ALL;
    constexpr uint32_t kSomeGroups = 0x3;

    {
      TestKTraceState state;
      ASSERT_TRUE(state.Init(kDefaultBufferSize, 0));

      // We didn't provide a non-zero initial set of groups, so the trace should
      // not be started right now.  Stopping, rewinding, and reading are all
      // legal (although, stopping does nothing).  We have not allocated our
      // buffer yet, so not even the static metadata should be available to
      // read.
      ASSERT_OK(state.Stop());
      ASSERT_EQ(0u, state.ReadUser(user_out_ptr<void>(nullptr), 0, 0));
      ASSERT_OK(state.Rewind());
      ASSERT_EQ(0u, state.grpmask());

      // Starting should succeed.
      ASSERT_OK(state.Start(kAllGroups, StartMode::Saturate));
      ASSERT_EQ(kAllGroups, state.grpmask());

      // Now that we are started, rewinding or should fail because of the state
      // check.
      ASSERT_EQ(ZX_ERR_BAD_STATE, state.Rewind());
      ASSERT_EQ(ZX_ERR_BAD_STATE, state.ReadUser(user_out_ptr<void>(nullptr), 0, 0));
      ASSERT_EQ(kAllGroups, state.grpmask());

      // Starting while already started should succeed, but change the active
      // group mask.
      ASSERT_OK(state.Start(kSomeGroups, StartMode::Saturate));
      ASSERT_EQ(kSomeGroups, state.grpmask());

      // Stopping is still OK, and actually does something now (it clears the
      // group mask).
      ASSERT_OK(state.Stop());
      ASSERT_EQ(0u, state.grpmask());

      // Now that we are stopped, we can read, rewind, and stop again.  Since we
      // have started before, we expect that the amount of data available to
      // read should be equal to the size of the two static metadata records.
      const ssize_t expected_size = sizeof(uint64_t) * 3;
      ASSERT_EQ(expected_size, state.ReadUser(user_out_ptr<void>(nullptr), 0, 0));
      ASSERT_OK(state.Rewind());
    }

    {
      // Same checks as before, but this time start in the started state after
      // init by providing a non-zero set of groups.
      TestKTraceState state;
      ASSERT_TRUE(state.Init(kDefaultBufferSize, kAllGroups));
      ASSERT_EQ(kAllGroups, state.grpmask());

      // We are started, so rewinding or reading should fail because of the
      // state check.
      ASSERT_EQ(ZX_ERR_BAD_STATE, state.Rewind());
      ASSERT_EQ(ZX_ERR_BAD_STATE, state.ReadUser(user_out_ptr<void>(nullptr), 0, 0));
      ASSERT_EQ(kAllGroups, state.grpmask());

      // "Restarting" should change the active group mask.
      ASSERT_OK(state.Start(kSomeGroups, StartMode::Saturate));
      ASSERT_EQ(kSomeGroups, state.grpmask());

      // Stopping should work.
      ASSERT_OK(state.Stop());
      ASSERT_EQ(0u, state.grpmask());

      // Stopping again, rewinding, and reading are all OK now.
      const ssize_t expected_size = sizeof(uint64_t) * 3;
      ASSERT_OK(state.Stop());
      ASSERT_EQ(expected_size, state.ReadUser(user_out_ptr<void>(nullptr), 0, 0));
      ASSERT_OK(state.Rewind());
      ASSERT_OK(state.Stop());
    }

    END_TEST;
  }

  static bool CircularWriteTest() {
    BEGIN_TEST;

    enum class Padding { Needed, NotNeeded };
    constexpr uint32_t kAllGroups = KTRACE_GRP_ALL;
    constexpr ktl::array kPasses = {Padding::Needed, Padding::NotNeeded};

    // Define a couple of lambdas and small amount of state which we will be
    // using to verify the contents of the trace buffer after writing different
    // patterns to it.
    //
    // The test will be conducted in two passes; one pass where we fill up our
    // buffer perfectly when we wrap, and another where we do not (and need a
    // padding record).  In both passes, we start by writing two records while
    // we are in "saturation" mode.  These records are part of the "static"
    // region of the trace and should always be present.  These static records
    // also have distinct patterns that we can look for when verifying the
    // contents of the trace.
    //
    // After writing these records and verifying the contents of the buffer, we
    // restart in circular mode and write just enough in the way of records to
    // fill the trace buffer, but not wrap it.  These new "circular" records
    // contain distinct payloads based on the order in which they were written
    // and can be used for verification.  In addition, we expect a padding
    // record to be present during the padding pass, but _only_ once and _only_
    // after we have forced the buffer to wrap.
    uint32_t record_count = 0;
    uint32_t expected_first_circular = 0;
    uint32_t enumerated_records = 0;
    bool saw_padding = false;

    auto ResetPayloadCheckerState = [&](uint32_t first_circular) {
      record_count = 0;
      expected_first_circular = first_circular;
      enumerated_records = 0;
      saw_padding = false;
    };

    for (const Padding pass : kPasses) {
      auto payload_checker = [&](const uint64_t* hdr) {
        BEGIN_TEST;

        ASSERT_NONNULL(hdr);
        const uint32_t len = fxt::RecordFields::RecordSize::Get<uint32_t>(*hdr);

        if (record_count == 0) {
          // Record number #0 should always be present, and always have a 0xAAAA'AAAA'AAAA'AAAA
          // timestamp
          ASSERT_EQ(4u, len);
          EXPECT_EQ(0xAAAA'AAAA'AAAA'AAAA, hdr[1]);
          // Argument data
          EXPECT_EQ(uint64_t{0x0000'0000'0003'0023}, hdr[2]);
          EXPECT_EQ(uint64_t{0x2222'2222'2222'2222}, hdr[3]);
        } else if (record_count == 1) {
          // Record number #1 should always be present, and will have a length
          // of 4 or 5 words, and a 0xBBBB'BBBB'BBBB'BBBB or 0xCCCC'CCCC'CCCC'CCCC payload,
          // depending on whether or not this pass of the test is one where we
          // expect to need a padding record or not.
          if (pass == Padding::Needed) {
            ASSERT_EQ(4u, len);
            EXPECT_EQ(uint64_t{0x0002'0001'0110'0044}, hdr[0]);
            EXPECT_EQ(uint64_t{0xBBBB'BBBB'BBBB'BBBB}, hdr[1]);
            EXPECT_EQ(uint64_t{0x0000'0000'0003'0023}, hdr[2]);
            EXPECT_EQ(uint64_t{0x3333'3333'3333'3333}, hdr[3]);
          } else {
            ASSERT_EQ(5u, len);
            EXPECT_EQ(uint64_t{0x0002'0001'0120'0054}, hdr[0]);
            EXPECT_EQ(uint64_t{0xCCCC'CCCC'CCCC'CCCC}, hdr[1]);
            EXPECT_EQ(uint64_t{0x0000'0000'0003'0023}, hdr[2]);
            EXPECT_EQ(uint64_t{0x4444'4444'4444'4444}, hdr[3]);
            EXPECT_EQ(uint64_t{0x4444'4444'0004'0011}, hdr[4]);
          }
        } else {
          // All subsequent records should either be a padding record, or a 32
          // byte record whose payload values are a function of their index.
          const uint32_t record_type = fxt::RecordFields::Type::Get<uint32_t>(*hdr);
          if (record_type != 0) {
            // A non-zero group indicates a normal record.
            const uint32_t ndx = record_count + expected_first_circular - 2;
            ASSERT_EQ(4u, len);
            EXPECT_EQ(uint64_t{0x0006'0005'0A10'0044}, hdr[0]);
            EXPECT_EQ(uint64_t{ndx}, hdr[1]);
            EXPECT_EQ(uint64_t{0x0000'0000'0007'0023}, hdr[2]);
            EXPECT_EQ(uint64_t{0x6666'6666'6666'6666}, hdr[3]);
          } else {
            // A record type 0 indicates a padding record.
            if (pass == Padding::Needed) {
              // should only ever see (at most) a single padding record per check.
              ASSERT_FALSE(saw_padding);
              saw_padding = true;
            } else {
              ASSERT_TRUE(false);  // we should not be seeing padding on a non-padding pass.
            }

            // Don't count padding records in the record count.
            --record_count;
          }
        }

        ++record_count;
        END_TEST;
      };

      // Allocate our trace buffer and auto-start it during init in non-circular
      // mode.
      TestKTraceState state;
      ASSERT_TRUE(state.Init(kDefaultBufferSize, kAllGroups));

      // In order to run this test, we need enough space in our buffer after the
      // 3 reserved metadata words for a least two "static" records, and a
      // small number of extra records.
      constexpr uint32_t kOverhead = sizeof(uint64_t) * 3;
      constexpr uint32_t kExtraRecords = 5;
      const uint32_t kStaticOverhead = 32 + ((pass == Padding::Needed) ? 32 : 40);
      ASSERT_GE(kDefaultBufferSize, (kOverhead + kStaticOverhead + (32 * kExtraRecords)));

      fxt::WriteInstantEventRecord(
          &state, 0xAAAA'AAAA'AAAA'AAAA, fxt::ThreadRef{0x01}, fxt::StringRef{1}, fxt::StringRef{2},
          fxt::Argument{fxt::StringRef{3}, int64_t{0x2222'2222'2222'2222}});
      if (pass == Padding::Needed) {
        // 8 bytes header, 8 bytes ts, 16 bytes int64 arg = 32 bytes
        ASSERT_NE(0u, (kDefaultBufferSize - (kOverhead + kStaticOverhead)) % 32);
        fxt::WriteInstantEventRecord(
            &state, 0xBBBB'BBBB'BBBB'BBBB, fxt::ThreadRef{0x01}, fxt::StringRef{1},
            fxt::StringRef{2}, fxt::Argument{fxt::StringRef{3}, int64_t{0x3333'3333'3333'3333}});
      } else {
        ASSERT_EQ(0u, (kDefaultBufferSize - (kOverhead + kStaticOverhead)) % 32);
        // 8 bytes header, 8 bytes ts, 16 bytes int64 arg, 8 bytes int32 arg = 40 bytes
        fxt::WriteInstantEventRecord(
            &state, 0xCCCC'CCCC'CCCC'CCCC, fxt::ThreadRef{0x01}, fxt::StringRef{1},
            fxt::StringRef{2}, fxt::Argument{fxt::StringRef{3}, int64_t{0x4444'4444'4444'4444}},
            fxt::Argument{fxt::StringRef{4}, int32_t{0x4444'4444}});
      }
      ASSERT_TRUE(state.CheckExpectedOffset(kOverhead + kStaticOverhead));

      // Stop the trace and verify that we have the records we expect.  Right
      // now, we expect to find only a pair of static records after the
      // metadata.
      const uint32_t kMaxCircular32bRecords =
          (kDefaultBufferSize - (kOverhead + kStaticOverhead)) / 32u;
      ASSERT_OK(state.Stop());
      ResetPayloadCheckerState(0);
      EXPECT_TRUE(state.TestAllRecords(enumerated_records, payload_checker));
      EXPECT_EQ(2u, enumerated_records);
      EXPECT_FALSE(saw_padding);

      // OK, now restart in circular mode, and write the maximum number of 32
      // byte records we can, without causing a wrap.
      ASSERT_OK(state.Start(kAllGroups, StartMode::Circular));
      for (uint32_t i = 0; i < kMaxCircular32bRecords; ++i) {
        const uint32_t ndx = i;
        EXPECT_EQ(ZX_OK,
                  fxt::WriteInstantEventRecord(
                      &state, ndx, fxt::ThreadRef{0x0A}, fxt::StringRef{5}, fxt::StringRef{6},
                      fxt::Argument{fxt::StringRef{7}, int64_t{0x6666'6666'6666'6666}}));
      }

      // Stop, and check the contents.
      ASSERT_OK(state.Stop());
      ResetPayloadCheckerState(0);
      EXPECT_TRUE(state.TestAllRecords(enumerated_records, payload_checker));
      EXPECT_EQ(2u + kMaxCircular32bRecords, enumerated_records);
      EXPECT_FALSE(saw_padding);

      // Start one last time, writing our extra records.  This should cause the
      // circular section of the ktrace buffer to wrap, requiring a padding
      // record if (and only if) this is the padding pass.  Our first "circular"
      // record should start with a payload index equal to the number of extra
      // records we wrote.
      ASSERT_OK(state.Start(kAllGroups, StartMode::Circular));
      for (uint32_t i = 0; i < kExtraRecords; ++i) {
        const uint32_t ndx = i + kMaxCircular32bRecords;
        EXPECT_EQ(ZX_OK,
                  fxt::WriteInstantEventRecord(
                      &state, ndx, fxt::ThreadRef{0x0A}, fxt::StringRef{5}, fxt::StringRef{6},
                      fxt::Argument{fxt::StringRef{7}, int64_t{0x6666'6666'6666'6666}}));
      }

      // Stop, and check the contents.
      ASSERT_OK(state.Stop());
      ResetPayloadCheckerState(kExtraRecords);
      EXPECT_TRUE(state.TestAllRecords(enumerated_records, payload_checker));
      if (pass == Padding::Needed) {
        EXPECT_EQ(2u + kMaxCircular32bRecords + 1u, enumerated_records);
        EXPECT_TRUE(saw_padding);
      } else {
        EXPECT_EQ(2u + kMaxCircular32bRecords, enumerated_records);
        EXPECT_FALSE(saw_padding);
      }
    }

    END_TEST;
  }

  static bool FxtCompatWriterTest() {
    BEGIN_TEST;

    constexpr uint32_t kAllGroups = KTRACE_GRP_ALL;

    // Create a small trace buffer and initialize it.
    TestKTraceState state;
    ASSERT_TRUE(state.Init(kDefaultBufferSize, kAllGroups));

    ASSERT_TRUE(state.CheckExpectedOffset(kInitialOffset));

    // This test works with the Reservation objects directly
    // rather than using the libfxt functions. Here we build a valid string
    // record in a convoluted way to cover various methods that libfxt uses to
    // write bytes.
    constexpr uint64_t fxt_header = 0x0000'0026'0001'0062;

    {
      auto reservation = state.Reserve(fxt_header);
      ASSERT_OK(reservation.status_value());
      reservation->WriteWord(0x6867'6665'6463'6261);
      reservation->WriteBytes("0123456789ABCDEF", 16);
      reservation->WriteBytes("remaining data", 14);
      reservation->Commit();
    }

    auto record_checker = [&](const uint64_t* hdr) {
      BEGIN_TEST;

      ASSERT_NONNULL(hdr);

      // KTrace length field should be computed from the FXT header.

      const char* fxt_start = reinterpret_cast<const char*>(hdr);

      EXPECT_EQ(0, memcmp(fxt_start,
                          "\x62\x00\x01\x00\x26\x00\x00\x00"
                          "abcdefgh"
                          "01234567"
                          "89ABCDEF"
                          "remainin"
                          "g data\0\0",
                          6 * sizeof(uint64_t)));

      END_TEST;
    };

    ASSERT_OK(state.Stop());

    uint32_t enumerated_records = 0;
    state.TestAllRecords(enumerated_records, record_checker);
    EXPECT_EQ(1u, enumerated_records);

    END_TEST;
  }

  // Check that Reserve() does not allocate any space in the trace
  // buffer in the case where writes were disabled before Reserve()
  // was called.  See https://fxbug.dev/42073175.
  static bool DisableWritesTest() {
    BEGIN_TEST;

    TestKTraceState state;
    ASSERT_TRUE(state.Init(kDefaultBufferSize, KTRACE_GRP_ALL));
    uint32_t expected_offset = kInitialOffset;
    ASSERT_TRUE(state.CheckExpectedOffset(expected_offset));

    constexpr uint64_t fxt_header = fxt::MakeHeader(fxt::RecordType::kMetadata, fxt::WordSize(1));

    // This initial Reserve() call should succeed and advance the
    // buffer offset.
    {
      zx::result<PendingCommit> reservation = state.Reserve(fxt_header);
      ASSERT_OK(reservation.status_value());
    }
    expected_offset += 8;
    ASSERT_TRUE(state.CheckExpectedOffset(expected_offset));
    ASSERT_EQ(0u, state.inflight_writes());

    // With writes disabled, this Reserve() call should return an
    // error without advancing the buffer offset.
    state.ClearMaskDisableWrites();
    {
      zx::result<PendingCommit> reservation = state.Reserve(fxt_header);
      ASSERT_EQ(reservation.status_value(), ZX_ERR_BAD_STATE);
    }
    ASSERT_TRUE(state.CheckExpectedOffset(expected_offset));
    ASSERT_EQ(0u, state.inflight_writes());

    ASSERT_OK(state.Stop());

    END_TEST;
  }

  // Begin a write (Reserve), but disable writes before the PendingCommit is
  // committed or destroyed.  Make sure we don't fail any asserts.  See
  // https://fxbug.dev/42073022.
  static bool DisableWritesDuringPendingCommitTest() {
    BEGIN_TEST;

    TestKTraceState state;
    ASSERT_TRUE(state.Init(kDefaultBufferSize, KTRACE_GRP_ALL));
    constexpr uint64_t fxt_header = fxt::MakeHeader(fxt::RecordType::kMetadata, fxt::WordSize(1));

    {
      zx::result<PendingCommit> reservation = state.Reserve(fxt_header);
      ASSERT_OK(reservation.status_value());
      reservation->WriteBytes("0123456789ABCDEF", 16);

      ASSERT_EQ(1u, state.inflight_writes());
      state.ClearMaskDisableWrites();
      reservation->Commit();
    }
    ASSERT_EQ(0u, state.inflight_writes());

    ASSERT_OK(state.Stop());

    END_TEST;
  }

 private:
  //////////////////////////////////////////////////////////////////////////////
  //
  // Actual class implementation starts here.  All members are private, the only
  // public members of the class are the static test hooks.
  //
  //////////////////////////////////////////////////////////////////////////////

  // clang-format off
  enum class CheckOp { LT, LE, EQ, GT, GE };
  // clang-format on

  TestKTraceState() {
    // disable diagnostic printfs in the test instances of KTrace we create.
    disable_diags_printfs_ = true;
  }

  // TODO(johngro): The default KTraceState implementation never cleans up its
  // buffer allocation, as it assumes that it is being used as a global
  // singleton, and that the kernel will never "exit". Test instances of
  // KTraceState *must* clean themselves up, however.  Should we push this
  // behavior down one level into the default KTraceState implementation's
  // destructor, even though it (currently) does not ever have any reason to
  // destruct?
  ~TestKTraceState() {
    if (buffer_ != nullptr) {
      VmAspace* aspace = VmAspace::kernel_aspace();
      aspace->FreeRegion(reinterpret_cast<vaddr_t>(buffer_));
    }
  }

  // We interpose ourselves in the Init path so that we can allocate the side
  // buffer we will use for validation.
  [[nodiscard]] bool Init(uint32_t target_bufsize, uint32_t initial_groups) {
    BEGIN_TEST;

    // Tests should always be allocating in units of page size.
    ASSERT_EQ(0u, target_bufsize & (PAGE_SIZE - 1));
    // Double init is not allowed.
    ASSERT_NULL(validation_buffer_.get());

    fbl::AllocChecker ac;
    validation_buffer_.reset(new (&ac) uint8_t[target_bufsize]);
    ASSERT_TRUE(ac.check());
    validation_buffer_size_ = target_bufsize;

    KTraceState::Init(target_bufsize, initial_groups);

    // Make sure that the buffer size we requested was allocated exactly.
    {
      Guard<TraceDisabledSpinLock, IrqSave> guard{&write_lock_};
      ASSERT_GE(target_bufsize, bufsize_);
    }

    END_TEST;
  }

  void ReportStaticNames() override {
    last_static_name_report_time_ = current_mono_time();
    ++static_name_report_count_;
  }

  void ReportThreadProcessNames() override {
    last_thread_name_report_time_ = current_mono_time();
    ++thread_name_report_count_;
  }

  zx_status_t CopyToUser(user_out_ptr<uint8_t> dst, const uint8_t* src, size_t len) override {
    ::memcpy(dst.get(), src, len);
    return ZX_OK;
  }

  // Check to make sure that the buffer is not operating in circular mode, and
  // that the write pointer is at the offset we expect.
  [[nodiscard]] bool CheckExpectedOffset(size_t expected, CheckOp op = CheckOp::EQ)
      TA_EXCL(write_lock_) {
    BEGIN_TEST;

    Guard<TraceDisabledSpinLock, IrqSave> guard{&write_lock_};
    switch (op) {
        // clang-format off
      case CheckOp::LT: EXPECT_LT(expected, wr_); break;
      case CheckOp::LE: EXPECT_LE(expected, wr_); break;
      case CheckOp::EQ: EXPECT_EQ(expected, wr_); break;
      case CheckOp::GT: EXPECT_GT(expected, wr_); break;
      case CheckOp::GE: EXPECT_GE(expected, wr_); break;
      // clang-format on
      default:
        ASSERT_TRUE(false);
    }
    EXPECT_EQ(0u, rd_);
    EXPECT_EQ(0u, circular_size_);

    END_TEST;
  }

  template <typename Checker>
  bool TestAllRecords(uint32_t& records_enumerated_out, const Checker& do_check)
      TA_EXCL(write_lock_) {
    BEGIN_TEST;
    // Make sure that we give a value to all of our out parameters.
    records_enumerated_out = 0;

    // Make sure that Read reports a reasonable size needed to read the buffer.
    ssize_t available = ReadUser(user_out_ptr<void>(nullptr), 0, 0);
    ASSERT_GE(available, 0);
    ASSERT_LE(static_cast<size_t>(available), validation_buffer_size_);

    // Now actually read the data, make sure that we read the same amount that
    // the size operation told us we would need to read.
    ASSERT_NONNULL(validation_buffer_.get());
    size_t to_validate =
        ReadUser(user_out_ptr<void>(validation_buffer_.get()), 0, validation_buffer_size_);
    ASSERT_EQ(static_cast<size_t>(available), to_validate);

    size_t rd_offset = sizeof(uint64_t) * 3;
    ASSERT_GE(to_validate, rd_offset);

    // We expect all trace buffers to start with a metadata records indicating the
    // version of the trace buffer format, and the clock resolution.  Verify that
    // these are present.
    const uint8_t* buffer = validation_buffer_.get();
    const uint64_t magic = reinterpret_cast<const uint64_t*>(buffer)[0];
    const uint64_t init_header = reinterpret_cast<const uint64_t*>(buffer)[1];
    const uint64_t fxt_ticks = reinterpret_cast<const uint64_t*>(buffer)[2];

    EXPECT_EQ(uint64_t{0x0016547846040010}, magic);

    // If something goes wrong while testing records, report _which_ record has
    // trouble, to assist with debugging.
    auto ReportRecord = fit::defer(
        [&]() { printf("\nFAILED while enumerating record (%u)\n", records_enumerated_out); });

    const uint64_t clock_res = ticks_per_second();
    EXPECT_EQ(uint64_t{0x21}, init_header);
    EXPECT_EQ(clock_res, fxt_ticks);

    while (rd_offset < to_validate) {
      const uint64_t* hdr = reinterpret_cast<const uint64_t*>(buffer + rd_offset);
      size_t record_size = fxt::RecordFields::RecordSize::Get<size_t>(*hdr);
      // Zero length records are not legal.
      ASSERT_GT(record_size, size_t{0});

      // Make sure the record matches expectations.
      ASSERT_TRUE(do_check(hdr));

      // Advance to the next record.
      ++records_enumerated_out;
      rd_offset += record_size * 8;
    }

    EXPECT_EQ(rd_offset, to_validate);

    ReportRecord.cancel();
    END_TEST;
  }

  zx_instant_mono_t last_static_name_report_time_{0};
  zx_instant_mono_t last_thread_name_report_time_{0};
  uint32_t static_name_report_count_{0};
  uint32_t thread_name_report_count_{0};

  ktl::unique_ptr<uint8_t[]> validation_buffer_;
  size_t validation_buffer_size_{0};
};

}  // namespace ktrace_tests

UNITTEST_START_TESTCASE(ktrace_tests)
UNITTEST("init/start", ktrace_tests::TestKTraceState::InitStartTest)
UNITTEST("write record", ktrace_tests::TestKTraceState::WriteRecordsTest)
UNITTEST("saturation", ktrace_tests::TestKTraceState::SaturationTest)
UNITTEST("rewind", ktrace_tests::TestKTraceState::RewindTest)
UNITTEST("state check", ktrace_tests::TestKTraceState::StateCheckTest)
UNITTEST("circular", ktrace_tests::TestKTraceState::CircularWriteTest)
UNITTEST("fxt compat writer", ktrace_tests::TestKTraceState::FxtCompatWriterTest)
UNITTEST("disable writes", ktrace_tests::TestKTraceState::DisableWritesTest)
UNITTEST("disable writes during pending commit",
         ktrace_tests::TestKTraceState::DisableWritesDuringPendingCommitTest)
UNITTEST_END_TESTCASE(ktrace_tests, "ktrace", "KTrace tests")
