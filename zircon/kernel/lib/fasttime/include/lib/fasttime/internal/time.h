// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed
// by a BSD-style license that can be found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_FASTTIME_INCLUDE_LIB_FASTTIME_INTERNAL_TIME_H_
#define ZIRCON_KERNEL_LIB_FASTTIME_INCLUDE_LIB_FASTTIME_INTERNAL_TIME_H_

#include <lib/affine/ratio.h>
#include <lib/arch/intrin.h>
#include <lib/fasttime/internal/abi.h>
#include <zircon/time.h>
#include <zircon/types.h>

namespace fasttime::internal {

struct SequentialReadResult {
  zx_ticks_t raw_ticks;
  zx_ticks_t obs1;
  zx_ticks_t obs2;
};

#if defined(__aarch64__)

#define TIMER_REG_CNTVCT "cntvct_el0"
#define TIMER_REG_CNTPCT "cntpct_el0"

inline zx_ticks_t get_raw_ticks_arm_a73_vct() {
  zx_ticks_t ticks1 = __arm_rsr64(TIMER_REG_CNTVCT);
  zx_ticks_t ticks2 = __arm_rsr64(TIMER_REG_CNTVCT);
  return (((ticks1 ^ ticks2) >> 32) & 1) ? ticks1 : ticks2;
}

inline zx_ticks_t get_raw_ticks_arm_a73_pct() {
  zx_ticks_t ticks1 = __arm_rsr64(TIMER_REG_CNTPCT);
  zx_ticks_t ticks2 = __arm_rsr64(TIMER_REG_CNTPCT);
  return (((ticks1 ^ ticks2) >> 32) & 1) ? ticks1 : ticks2;
}

inline zx_ticks_t get_raw_ticks(const TimeValues& tvalues) {
  if (tvalues.use_a73_errata_mitigation) {
    return tvalues.use_pct_instead_of_vct ? get_raw_ticks_arm_a73_pct()
                                          : get_raw_ticks_arm_a73_vct();
  } else {
    return tvalues.use_pct_instead_of_vct ? __arm_rsr64(TIMER_REG_CNTPCT)
                                          : __arm_rsr64(TIMER_REG_CNTVCT);
  }
}

// Reads the raw ticks value in between two observations of the given atomic value.
inline SequentialReadResult read_raw_ticks_sequentially(const TimeValues& tvalues,
                                                        const std::atomic<zx_ticks_t>& value) {
  SequentialReadResult result = {0, ZX_TIME_INFINITE, ZX_TIME_INFINITE_PAST};
  // This assembly block reads the offset and ensures that this read must complete before the read
  // of the virtual ticks counter below. The method is derived from Example D12-3 in the 'Arm
  // Architecture Reference Manual for A-profile architecture' revision 'ARM DDI 0487K.a'. It works
  // by establishing a data dependency between the offset and the counter read to ensure that the
  // processor cannot reorder these instructions. It does so by:
  // 1. Loading the value into result.obs1.
  // 2. EOR'ing obs1 with itself to generate a zero.
  // 3. Using a CBZ to branch on the generated zero. We as humans know that this is an
  //    unconditional branch because the input register will always be zero, but the CPU does not
  //    know that and therefore cannot reorder the load w.r.t to the CBZ.
  // 4. Issuing an ISB after the branch. This isb prevents the CBZ from being reordered down.
  uint64_t data_dependent_zero;
  __asm__ volatile(
      R"""(
      ldr %[obs1], %[value_ref]
      eor %[data_dependent_zero], %[obs1], %[obs1]
      cbz %[data_dependent_zero], 1f
      1: isb
      )"""
      : [data_dependent_zero] "=r"(data_dependent_zero),
        [obs1] "=r"(result.obs1)  // Outputs: obs1 stores value, and the results of the EOR are
                                  // written into data_dependent_zero.
      : [value_ref] "m"(value)    // Inputs: value is read from memory into obs1.
  );

  // This ticks must not be reordered with the assembly blocks before or after it.
  // We achieve this by using the __arm_rsr64 intrinsics in get_raw_ticks, which use the volatile
  // qualifier. This should ensure that the blocks are not reordered. Furthermore, the block below
  // establishes a data dependency on result.raw_ticks.
  result.raw_ticks = get_raw_ticks(tvalues);

  // This assembly block reads the offset a second time, ensuring that this read cannot begin until
  // the previous ticks counter read(s) complete. The method used comes from Example D12-4 in the
  // 'Arm Architecture Reference Manual for A-profile architecture' revision 'ARM DDI 0487K.a'.
  // It works by:
  // 1. EOR'ing result.raw_ticks with itself to generate a zero.
  // 2. Loading the value but doing so by adding the generated zero offset to the load address.
  //    Once again, this will just always load value, but the CPU does not know that and therefore
  //    cannot reorder the ticks read with the ldr.
  __asm__ volatile(
      R"""(
      eor %[data_dependent_zero], %[ticks], %[ticks]
      ldr %[obs2], [%[value_ptr], %[data_dependent_zero]] // Same as %[value_ref]
      )"""
      : [data_dependent_zero] "=&r"(data_dependent_zero),
        [obs2] "=r"(result.obs2)  // Outputs:  obs2 stores value, and the results of the EOR are
                                  // writing into data_dependent_zero. We use the early clobber
                                  // here to ensure that the same register is not used for it and
                                  // value_ptr.
      : [ticks] "r"(result.raw_ticks), [value_ref] "m"(value),
        [value_ptr] "r"(&value)  // Inputs: raw_ticks is used as input to to EOR. value_ptr is read
                                 // into obs2. value is used in the comment.
  );
  return result;
}

#elif defined(__x86_64__)

inline zx_ticks_t get_raw_ticks(const TimeValues& tvalues) { return __rdtsc(); }

// Reads the raw ticks value in between two observations of the given atomic value.
inline SequentialReadResult read_raw_ticks_sequentially(const TimeValues& tvalues,
                                                        const std::atomic<zx_ticks_t>& value) {
  SequentialReadResult result = {0, ZX_TIME_INFINITE, ZX_TIME_INFINITE_PAST};
  result.obs1 = value.load(std::memory_order_relaxed);

  // This sequential read utilizes a rdtscp instruction to ensure that all previous loads are
  // globally visible. It then emits an lfence immediately after to ensure that the rdtscp is
  // executed prior to the execution of any subsequent instruction. This method comes from the
  // "RDTSCP—Read Time-Stamp Counter and Processor ID" section of the Intel Software Developer's
  // Manual Volume 2B revision 4-557.
  unsigned int unused;
  result.raw_ticks = __rdtscp(&unused);

  // Use an lfence to ensure that the subsequent load does not start until the rdtscp is complete.
  _mm_lfence();

  result.obs2 = value.load(std::memory_order_relaxed);
  return result;
}

#elif defined(__riscv)

// Reads the raw ticks value in between two observations of the given atomic value.
inline SequentialReadResult read_raw_ticks_sequentially(const TimeValues& tvalues,
                                                        const std::atomic<zx_ticks_t>& value) {
  SequentialReadResult result = {0, ZX_TIME_INFINITE, ZX_TIME_INFINITE_PAST};
  result.obs1 = value.load(std::memory_order_relaxed);
  // TODO(https://fxbug.dev/383385811): Research is required in order to properly synchronize
  // observations of the RISC-V system timer against the instruction pipeline. Once we know the
  // methods needed to do so, apply them here.
  __asm__ volatile("rdtime %0" : "=r"(result.raw_ticks));
  result.obs2 = value.load(std::memory_order_relaxed);
  return result;
}

inline zx_ticks_t get_raw_ticks(const TimeValues& tvalues) {
  zx_ticks_t ticks;
  __asm__ volatile("rdtime %0" : "=r"(ticks));
  return ticks;
}

#else

#error Unsupported architecture

#endif

constexpr uint64_t kFasttimeVersion = 1;

enum class FasttimeVerificationMode : uint8_t {
  kNormal,
  kSkip,
};

inline bool check_fasttime_version(const TimeValues& tvalues) {
  return tvalues.version == kFasttimeVersion;
}

template <FasttimeVerificationMode kVerificationMode = FasttimeVerificationMode::kNormal>
inline zx_ticks_t compute_monotonic_ticks(const TimeValues& tvalues) {
  if constexpr (kVerificationMode == FasttimeVerificationMode::kNormal) {
    if (!tvalues.usermode_can_access_ticks || tvalues.version != kFasttimeVersion) {
      return ZX_TIME_INFINITE_PAST;
    }
  }
  while (true) {
    // TODO(https://fxbug.dev/341785588): The get_raw_ticks call here does not correctly
    // enforce ordering. This should be fixed before we suspend the system.
    const zx_ticks_t obs1 = tvalues.mono_ticks_offset.load(std::memory_order_relaxed);
    const zx_ticks_t raw_ticks = internal::get_raw_ticks(tvalues);
    const zx_ticks_t obs2 = tvalues.mono_ticks_offset.load(std::memory_order_relaxed);
    if (obs1 == obs2) {
      return raw_ticks + obs1;
    }
  }
}

template <FasttimeVerificationMode kVerificationMode = FasttimeVerificationMode::kNormal>
inline zx_time_t compute_monotonic_time(const TimeValues& tvalues) {
  const zx_ticks_t ticks = compute_monotonic_ticks<kVerificationMode>(tvalues);
  if constexpr (kVerificationMode == FasttimeVerificationMode::kNormal) {
    if (ticks == ZX_TIME_INFINITE_PAST) {
      return ticks;
    }
  }
  const affine::Ratio ticks_to_time_ratio(tvalues.ticks_to_time_numerator,
                                          tvalues.ticks_to_time_denominator);
  return ticks_to_time_ratio.Scale(ticks);
}

template <FasttimeVerificationMode kVerificationMode = FasttimeVerificationMode::kNormal>
inline zx_ticks_t compute_boot_ticks(const TimeValues& tvalues) {
  if constexpr (kVerificationMode == FasttimeVerificationMode::kNormal) {
    if (!tvalues.usermode_can_access_ticks || tvalues.version != kFasttimeVersion) {
      return ZX_TIME_INFINITE_PAST;
    }
  }
  return internal::get_raw_ticks(tvalues) + tvalues.boot_ticks_offset;
}

template <FasttimeVerificationMode kVerificationMode = FasttimeVerificationMode::kNormal>
inline zx_time_t compute_boot_time(const TimeValues& tvalues) {
  const zx_ticks_t ticks = compute_boot_ticks<kVerificationMode>(tvalues);
  if constexpr (kVerificationMode == FasttimeVerificationMode::kNormal) {
    if (ticks == ZX_TIME_INFINITE_PAST) {
      return ticks;
    }
  }
  // The scaling factor from raw ticks to boot ticks is the same as that from raw ticks to mono
  // ticks.
  const affine::Ratio ticks_to_time_ratio(tvalues.ticks_to_time_numerator,
                                          tvalues.ticks_to_time_denominator);
  return ticks_to_time_ratio.Scale(ticks);
}

}  // namespace fasttime::internal

#endif  // ZIRCON_KERNEL_LIB_FASTTIME_INCLUDE_LIB_FASTTIME_INTERNAL_TIME_H_
