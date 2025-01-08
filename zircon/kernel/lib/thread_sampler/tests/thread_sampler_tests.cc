// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/fit/defer.h>
#include <lib/fxt/serializer.h>
#include <lib/thread_sampler/thread_sampler.h>
#include <lib/unittest/unittest.h>

#include <ktl/algorithm.h>
#include <ktl/limits.h>
#include <ktl/unique_ptr.h>
#include <vm/vm_aspace.h>

#include "kernel/mp.h"
#include "lib/zx/time.h"

#include <ktl/enforce.h>

namespace thread_sampler_tests {

// A test version of ThreadSampler which overrides functions
// for testing purposes.
class TestThreadSampler : public sampler::ThreadSamplerDispatcher {
 public:
  TestThreadSampler(fbl::RefPtr<PeerHolder<IoBufferDispatcher>> holder, IobEndpointId endpoint_id,
                    fbl::RefPtr<SharedIobState> shared_state)
      : sampler::ThreadSamplerDispatcher(ktl::move(holder), endpoint_id, ktl::move(shared_state)) {}

  void SampleThread(zx_koid_t pid, zx_koid_t tid, GeneralRegsSource source, void* gregs) {
    sampler::internal::PerCpuState& cpu_state = GetPerCpuState(arch_curr_cpu_num());
    bool enabled = cpu_state.SetPendingWrite();
    if (!enabled) {
      return;
    }
    auto d = fit::defer([&cpu_state]() { cpu_state.ResetPendingWrite(); });

    constexpr size_t kMaxUserBacktraceSize = 64;
    vaddr_t bt[kMaxUserBacktraceSize]{};
    for (unsigned i = 0; i < kMaxUserBacktraceSize; ++i) {
      bt[i] = i;
    }

    constexpr fxt::StringRef<fxt::RefType::kId> empty_string{0};
    const fxt::ThreadRef current_thread{pid, tid};
    fxt::WriteLargeBlobRecordWithMetadata(&cpu_state, current_ticks(), empty_string, empty_string,
                                          current_thread, bt,
                                          sizeof(uint64_t) * kMaxUserBacktraceSize);
  }

  static bool RepeatStartStopTest() {
    BEGIN_TEST;
    {
      // Construct a thread sampler state and initialize it
      zx_sampler_config_t config{
          .period = zx::msec(1).get(),
          .buffer_size = ZX_PAGE_SIZE,
      };
      KernelHandle<sampler::ThreadSamplerDispatcher> state;
      for (int i = 0; i < 10; i++) {
        KernelHandle<sampler::ThreadSamplerDispatcher> read_handle;
        ASSERT_TRUE(ThreadSamplerDispatcher::CreateImpl(config, read_handle, state).is_ok());
        auto test_state = fbl::RefPtr<TestThreadSampler>::Downcast(state.release());
        ASSERT_TRUE(test_state->StartImpl().is_ok());
        ASSERT_TRUE(test_state->StopImpl().is_ok());
      }

      // We should also be able to drop the read handle without stopping first and the state should
      // get cleaned up properly
      for (int i = 0; i < 10; i++) {
        KernelHandle<sampler::ThreadSamplerDispatcher> read_handle;
        ASSERT_TRUE(ThreadSamplerDispatcher::CreateImpl(config, read_handle, state).is_ok());
        auto test_state = fbl::RefPtr<TestThreadSampler>::Downcast(state.release());
        ASSERT_TRUE(test_state->StartImpl().is_ok());
      }
    }

    END_TEST;
  }
  static bool WriteSampleTest() {
    BEGIN_TEST;
    {
      // Construct a thread sampler state and initialize it
      KernelHandle<sampler::ThreadSamplerDispatcher> state;
      KernelHandle<sampler::ThreadSamplerDispatcher> read_handle;

      zx_sampler_config_t config{
          .period = zx::msec(1).get(),
          .buffer_size = ZX_PAGE_SIZE,
      };
      ASSERT_TRUE(ThreadSamplerDispatcher::CreateImpl(config, read_handle, state).is_ok());

      auto test_state = fbl::RefPtr<TestThreadSampler>::Downcast(state.release());

      ASSERT_TRUE(test_state->StartImpl().is_ok());

      zx_instant_mono_ticks_t before = current_ticks();
      //  Write some fake samples to each buffer on each cpu
      mp_sync_exec(
          MP_IPI_TARGET_ALL, 0,
          [](void* s) {
            auto test_thread_sampler = reinterpret_cast<TestThreadSampler*>(s);
            test_thread_sampler->SampleThread(arch_curr_cpu_num(), 1, GeneralRegsSource::None,
                                              nullptr);
          },
          test_state.get());
      zx_instant_mono_ticks_t after = current_ticks();
      ASSERT_TRUE(test_state->StopImpl().is_ok());

      // We should now be able to read the records
      size_t num_cpus = arch_max_num_cpus();
      for (unsigned i = 0; i < num_cpus; ++i) {
        auto vmo = test_state->GetVmo(i);
        // num_words = 64 backtrace + 1 large_header + 1 metadata + 1 ts + 1 inline pid + 1 inline
        // tid + 1 blob size = 70
        constexpr size_t num_words = 70;
        // We should see a large blob
        constexpr uint64_t large_blob_header =
            fxt::MakeLargeHeader(fxt::LargeRecordType::kBlob, fxt::WordSize(num_words));
        fxt::LargeBlobFields::BlobFormat::Make(ToUnderlyingType(fxt::LargeBlobFormat::kMetadata));
        uint64_t record[71];
        ASSERT_OK(vmo->Read(record, 0, 71 * sizeof(uint64_t)));
        EXPECT_EQ(large_blob_header, record[0]);
        // 0 arguments, inline thread ref, and empty name/category
        EXPECT_EQ(uint64_t{0}, record[1]);

        // timestamp
        EXPECT_GE(record[2], static_cast<uint64_t>(before));
        EXPECT_LE(record[2], static_cast<uint64_t>(after));

        // We wrote the cpu number as the pid
        EXPECT_EQ(i, record[3]);
        // And 1 as the tid
        EXPECT_EQ(uint64_t{1}, record[4]);
        // Blob size
        EXPECT_EQ(record[5], uint64_t{64} * sizeof(uint64_t));
        for (unsigned frame = 0; frame < 64; frame++) {
          EXPECT_EQ(record[6 + frame], frame);
        }
        // This should be one past our record should have been 0 allocated
        EXPECT_EQ(record[70], uint64_t{0});
      }
    }

    END_TEST;
  }
};
}  // namespace thread_sampler_tests

UNITTEST_START_TESTCASE(thread_sampler_tests)
UNITTEST("init/start", thread_sampler_tests::TestThreadSampler::RepeatStartStopTest)
UNITTEST("read/write", thread_sampler_tests::TestThreadSampler::WriteSampleTest)
UNITTEST_END_TESTCASE(thread_sampler_tests, "thread_sampler", "Thread Sampler tests")
