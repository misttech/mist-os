// Copyright 2016 The Fuchsia Authors
// Copyright (c) 2013, Google Inc. All rights reserved.
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <assert.h>
#include <inttypes.h>
#include <lib/affine/ratio.h>
#include <lib/arch/intrin.h>
#include <lib/arch/ticks.h>
#include <lib/boot-options/boot-options.h>
#include <lib/counters.h>
#include <lib/fit/defer.h>
#include <lib/fixed_point.h>
#include <lib/unittest/unittest.h>
#include <lib/zbi-format/driver-config.h>
#include <platform.h>
#include <pow2.h>
#include <trace.h>
#include <zircon/types.h>

#include <arch/interrupt.h>
#include <arch/quirks.h>
#include <dev/interrupt.h>
#include <dev/timer/arm_generic.h>
#include <kernel/scheduler.h>
#include <ktl/atomic.h>
#include <ktl/limits.h>
#include <lk/init.h>
#include <phys/handoff.h>
#include <platform/boot_timestamps.h>
#include <platform/timer.h>

#include <ktl/enforce.h>

#define LOCAL_TRACE 0

// AArch64 timer control registers
#define TIMER_REG_CNTKCTL "cntkctl_el1"
#define TIMER_REG_CNTFRQ "cntfrq_el0"

// CNTP AArch64 registers
#define TIMER_REG_CNTP_CTL "cntp_ctl_el0"
#define TIMER_REG_CNTP_CVAL "cntp_cval_el0"
#define TIMER_REG_CNTP_TVAL "cntp_tval_el0"
#define TIMER_REG_CNTPCT "cntpct_el0"

// CNTPS "AArch64" registers
#define TIMER_REG_CNTPS_CTL "cntps_ctl_el1"
#define TIMER_REG_CNTPS_CVAL "cntps_cval_el1"
#define TIMER_REG_CNTPS_TVAL "cntps_tval_el1"

// CNTV "AArch64" registers
#define TIMER_REG_CNTV_CTL "cntv_ctl_el0"
#define TIMER_REG_CNTV_CVAL "cntv_cval_el0"
#define TIMER_REG_CNTV_TVAL "cntv_tval_el0"
#define TIMER_REG_CNTVCT "cntvct_el0"

arch::EarlyTicks kernel_entry_ticks;
arch::EarlyTicks kernel_virtual_entry_ticks;

KCOUNTER(platform_timer_set_counter, "platform.timer.set")
KCOUNTER(platform_timer_cancel_counter, "platform.timer.cancel")

namespace {

// Counter-timer Kernel Control Register, EL1.
static constexpr uint64_t CNTKCTL_EL1_ENABLE_PHYSICAL_COUNTER = 1 << 0;
static constexpr uint64_t CNTKCTL_EL1_ENABLE_VIRTUAL_COUNTER = 1 << 1;

// Global saved config state
int timer_irq;
uint32_t timer_cntfrq;  // Timer tick rate in Hz.

enum timer_irq_assignment {
  IRQ_PHYS,
  IRQ_VIRT,
  IRQ_SPHYS,
};

timer_irq_assignment timer_assignment;

// event stream state
uint32_t event_stream_shift;
uint32_t event_stream_freq;

// Definition of the function signature we use to fetch the value of the chosen
// reference counter.
using ReadArmCounterFunc = uint64_t();

}  // anonymous namespace

static uint32_t read_cntp_ctl() { return __arm_rsr(TIMER_REG_CNTP_CTL); }
static uint32_t read_cntv_ctl() { return __arm_rsr(TIMER_REG_CNTV_CTL); }
static uint32_t read_cntps_ctl() { return __arm_rsr(TIMER_REG_CNTPS_CTL); }

static void write_cntp_ctl(uint32_t val) {
  LTRACEF_LEVEL(3, "cntp_ctl: 0x%x %x\n", val, read_cntp_ctl());
  __arm_wsr(TIMER_REG_CNTP_CTL, val);
  __isb(ARM_MB_SY);
}

static void write_cntv_ctl(uint32_t val) {
  LTRACEF_LEVEL(3, "cntv_ctl: 0x%x %x\n", val, read_cntv_ctl());
  __arm_wsr(TIMER_REG_CNTV_CTL, val);
  __isb(ARM_MB_SY);
}

static void write_cntps_ctl(uint32_t val) {
  LTRACEF_LEVEL(3, "cntps_ctl: 0x%x %x\n", val, read_cntps_ctl());
  __arm_wsr(TIMER_REG_CNTPS_CTL, val);
  __isb(ARM_MB_SY);
}

static void write_cntp_cval(uint64_t val) {
  LTRACEF_LEVEL(3, "cntp_cval: 0x%016" PRIx64 ", %" PRIu64 "\n", val, val);
  __arm_wsr64(TIMER_REG_CNTP_CVAL, val);
  __isb(ARM_MB_SY);
}

static void write_cntv_cval(uint64_t val) {
  LTRACEF_LEVEL(3, "cntv_cval: 0x%016" PRIx64 ", %" PRIu64 "\n", val, val);
  __arm_wsr64(TIMER_REG_CNTV_CVAL, val);
  __isb(ARM_MB_SY);
}

static void write_cntps_cval(uint64_t val) {
  LTRACEF_LEVEL(3, "cntps_cval: 0x%016" PRIx64 ", %" PRIu64 "\n", val, val);
  __arm_wsr64(TIMER_REG_CNTPS_CVAL, val);
  __isb(ARM_MB_SY);
}

static void write_cntp_tval(int32_t val) {
  LTRACEF_LEVEL(3, "cntp_tval: %d\n", val);
  __arm_wsr(TIMER_REG_CNTP_TVAL, val);
  __isb(ARM_MB_SY);
}

static void write_cntv_tval(int32_t val) {
  LTRACEF_LEVEL(3, "cntv_tval: %d\n", val);
  __arm_wsr(TIMER_REG_CNTV_TVAL, val);
  __isb(ARM_MB_SY);
}

static void write_cntps_tval(int32_t val) {
  LTRACEF_LEVEL(3, "cntps_tval: %d\n", val);
  __arm_wsr(TIMER_REG_CNTPS_TVAL, val);
  __isb(ARM_MB_SY);
}

// fwd decls to ensure that the read counter function all match the signature
// defined by ReadArmCounterFunc.
static ReadArmCounterFunc read_cntpct_a73;
static ReadArmCounterFunc read_cntvct_a73;
static ReadArmCounterFunc read_cntpct;
static ReadArmCounterFunc read_cntvct;

static uint64_t read_cntpct_a73() {
  // Workaround for Cortex-A73 erratum 858921.
  // Fix will be applied to all cores, as two consecutive reads should be
  // faster than checking if core is A73 and branching before every read.
  const uint64_t old_read = __arm_rsr64(TIMER_REG_CNTPCT);
  const uint64_t new_read = __arm_rsr64(TIMER_REG_CNTPCT);

  return (((old_read ^ new_read) >> 32) & 1) ? old_read : new_read;
}

static uint64_t read_cntvct_a73() {
  // Workaround for Cortex-A73 erratum 858921.
  // Fix will be applied to all cores, as two consecutive reads should be
  // faster than checking if core is A73 and branching before every read.
  const uint64_t old_read = __arm_rsr64(TIMER_REG_CNTVCT);
  const uint64_t new_read = __arm_rsr64(TIMER_REG_CNTVCT);

  return (((old_read ^ new_read) >> 32) & 1) ? old_read : new_read;
}

static uint64_t read_cntpct() { return __arm_rsr64(TIMER_REG_CNTPCT); }
static uint64_t read_cntvct() { return __arm_rsr64(TIMER_REG_CNTVCT); }

struct timer_reg_procs {
  void (*write_ctl)(uint32_t val);
  void (*write_cval)(uint64_t val);
  void (*write_tval)(int32_t val);
  uint64_t arch::EarlyTicks::* early_ticks;
};

[[maybe_unused]] static const struct timer_reg_procs cntp_procs = {
    .write_ctl = write_cntp_ctl,
    .write_cval = write_cntp_cval,
    .write_tval = write_cntp_tval,
    .early_ticks = &arch::EarlyTicks::cntpct_el0,
};

[[maybe_unused]] static const struct timer_reg_procs cntv_procs = {
    .write_ctl = write_cntv_ctl,
    .write_cval = write_cntv_cval,
    .write_tval = write_cntv_tval,
    .early_ticks = &arch::EarlyTicks::cntvct_el0,
};

[[maybe_unused]] static const struct timer_reg_procs cntps_procs = {
    .write_ctl = write_cntps_ctl,
    .write_cval = write_cntps_cval,
    .write_tval = write_cntps_tval,
    .early_ticks = &arch::EarlyTicks::cntpct_el0,
};

// Notes about the `read_arm_counter` function pointer:
//
// There exists a bug in certain ARM Cortex-A73 CPUs which can lead to a bad
// read of either the VCT or PCT counters.  It is documented as Errata 858921
// ( https://documentation-service.arm.com/static/5fa29fa7b209f547eebd3613 )
//
// At startup time, we have not had a chance (yet) to figure out what CPUs we
// are running on.  So, we start by using the read methods which implement the
// workaround for errata-858921; they are safe to use no matter what CPU you are
// on, they are just a bit slower.  Later on, once we have had a chance for all
// of our secondary CPUs to boot, identify their HW, and check in with the rest
// of the system, we can switch to the faster version (but only if there are no
// A73s present in the system).
//
// In order to make this switch, however, and not need any locks, we need to
// make sure that the function pointer is declared as an atomic.  Otherwise, we
// could be writing to the pointer when someone else is reading it (in order to
// read the clock) which would be a formal data race.  Note that we don't need
// any ordering of the loads and stores of the function pointer beyond
// `memory_order_relaxed`.  It is not important that we establish a specific
// order of the pointer's value relative to other memory accesses in the system.
// We just need to make sure that _if_ we decide to switch to the faster version
// of the counter read, that all of the CPUs _eventually_ see the pointer
// update (which they will do because of unavoidable synchronizing events like
// taking exceptions).
//
#if (TIMER_ARM_GENERIC_SELECTED_CNTV)
static struct timer_reg_procs reg_procs = cntv_procs;
static ktl::atomic<ReadArmCounterFunc*> read_arm_counter{read_cntvct_a73};
#else
static struct timer_reg_procs reg_procs = cntp_procs;
static ktl::atomic<ReadArmCounterFunc*> read_arm_counter{read_cntpct_a73};
#endif

static inline void write_ctl(uint32_t val) { reg_procs.write_ctl(val); }
static inline void write_cval(uint64_t val) { reg_procs.write_cval(val); }
[[maybe_unused]] static inline void write_tval(uint32_t val) { reg_procs.write_tval(val); }

static void platform_tick(void* arg) {
  write_ctl(0);
  timer_tick();
}

template <GetTicksSyncFlag Flags>
inline zx_ticks_t platform_current_raw_ticks_synchronized() {
  // Make certain that any reads of the raw system timer are guaranteed to take
  // place in a region defined by the template |Flags|.  Note that the
  // methodology used here was defined in
  //
  // 'Arm Architecture Reference Manual for A-profile architecture'
  // revision 'ARM DDI 0487K.a'
  //
  // In particular, please refer to examples D12-3 and D12-4.  Note that we
  // chose to use the "DMB and Branch Dependency" approach (instead of a DSB) to
  // ensure that timer reads take place after all previous memory accesses, and
  // we use a load dependent on the value of the timer load to ensure that the
  // timer load takes place before subsequent memory accesses.  Additionally, we
  // do not make any attempt to use self-synchronizing timer register accesses.
  // Refer to the cited examples for the potentially valid sequences.
  uint64_t temp;

  // Do we need to guarantee that this clock read occurs after previous loads,
  // stores, or both?
  //
  // If so, we need to implement the solution described in refer to Example
  // D12-4 in the ARM ARM, referenced above.
  //
  // We need our timer read to happen after all previous memory accesses.
  // This is implemented as a DMB followed by read from any valid memory
  // location with a "branch" which depends on that read.  Note that
  // "branch" is in air quotes because its target is the next instruction,
  // so whether or not the branch gets taken, the result is the same (to
  // execute the next instruction).  Finally, the sequence ends with an ISB.
  //
  // Also note that we need the DMB approach here, even when we only care about
  // previous loads, because we are attempting to ensure that our counter read
  // takes place after _all_ previous loads.  If we were interested in only
  // ensuring that the counter observation took place after all previous loads
  // of a _specific_ variable (call it `X`), there would be another option to
  // consider.  It is shown in example D12-3 of the ARM ARM, and involves
  // creating a branch which depends on the previously loaded value of `X`,
  // followed by an ISB.  Since the spec of this function is to ensure that the
  // counter observation follow _all_ previous loads, it is not a technique we
  // can use here.
  //
  // TODO(johngro): Consider adding an API for a raw counter read which would
  // allow for this.  There are two key cases (reading a synthetic kernel clock,
  // and reading the monotonic or boot timeline) where we don't actually need to
  // ensure that our counter read takes place after all previous loads, just the
  // previous load of a specific variable (the generation counter in the case of
  // a synthetic clock, and the offset in the case of a mono/boot timeline
  // read).  That said, designing such an API presents some challenges as not
  // all of the specific variable reads have the same requirements.  For
  // example, in the case of synthetic kernel clocks, the extra variable needs
  // to be read with acquire semantics.  In the case of mono/boot reads, relaxed
  // semantics are all that are needed.
  //
  constexpr bool must_read_after_all_previous_accesses =
      (Flags & (GetTicksSyncFlag::kAfterPreviousLoads | GetTicksSyncFlag::kAfterPreviousStores)) !=
      GetTicksSyncFlag::kNone;
  if constexpr (must_read_after_all_previous_accesses) {
    // We need our timer read to happen after all previous memory accesses.
    // This is implemented as a DMB followed by read from any valid memory
    // location with a "branch" which depends on that read.  Note that
    // "branch" is in air quotes because its target is the next instruction,
    // so whether or not the branch gets taken, the result is the same (to
    // execute the next instruction).  Finally, the sequence ends with an ISB.
    //
    // Refer to Example D12-4 in the ARM ARM referenced above.
    //
    __asm__ volatile(
        "dmb sy;"
        "ldr %[temp], [sp];"
        "cbz %[temp], 1f;"
        "1: isb;"
        : [temp] "=r"(temp)  // outputs  : we overwrite the register selected for "temp"
        :                    // inputs   : we have no inputs
        : "memory");         // clobbers : nothing, however we specify "memory" in order to
                             // prevent re-ordering, as a signal fence would do.
  }

  // Now actually read from the configured system timer.
  const zx_ticks_t ret = read_arm_counter.load(ktl::memory_order_relaxed)();

  // Do we need to guarantee that this clock read occurs before subsequent loads,
  // stores, or both?  If so, the recipe is the same in all cases.  We introduce
  // a load operation which has data dependency on ret, forcing the timer read
  // to finish before the dependent load can occur.
  //
  // Refer to Example D12-4 in the ARM ARM referenced above.
  //
  constexpr bool must_read_before_any_subsequent_access =
      (Flags & (GetTicksSyncFlag::kBeforeSubsequentLoads |
                GetTicksSyncFlag::kBeforeSubsequentStores)) != GetTicksSyncFlag::kNone;
  if constexpr (must_read_before_any_subsequent_access) {
    __asm__ volatile(
        "eor %[temp], %[ret], %[ret];"
        "ldr %[temp], [sp, %[temp]];"
        : [temp] "=&r"(temp)  // outputs  : we overwrite the register selected for "temp"
        : [ret] "r"(ret)      // inputs   : we consume the register holding |ret|
        : "cc", "memory");    // clobbers : EOR will clobber our flags, "memory" to prevent
                              // re-ordering.
  }

  return ret;
}

// Explicit instantiation of all of the forms of synchronized tick access.
#define EXPAND_PLATFORM_CURRENT_RAW_TICKS_SYNCHRONIZED(flags) \
  template zx_ticks_t                                         \
  platform_current_raw_ticks_synchronized<static_cast<GetTicksSyncFlag>(flags)>()
EXPAND_PLATFORM_CURRENT_RAW_TICKS_SYNCHRONIZED(0);
EXPAND_PLATFORM_CURRENT_RAW_TICKS_SYNCHRONIZED(1);
EXPAND_PLATFORM_CURRENT_RAW_TICKS_SYNCHRONIZED(2);
EXPAND_PLATFORM_CURRENT_RAW_TICKS_SYNCHRONIZED(3);
EXPAND_PLATFORM_CURRENT_RAW_TICKS_SYNCHRONIZED(4);
EXPAND_PLATFORM_CURRENT_RAW_TICKS_SYNCHRONIZED(5);
EXPAND_PLATFORM_CURRENT_RAW_TICKS_SYNCHRONIZED(6);
EXPAND_PLATFORM_CURRENT_RAW_TICKS_SYNCHRONIZED(7);
EXPAND_PLATFORM_CURRENT_RAW_TICKS_SYNCHRONIZED(8);
EXPAND_PLATFORM_CURRENT_RAW_TICKS_SYNCHRONIZED(9);
EXPAND_PLATFORM_CURRENT_RAW_TICKS_SYNCHRONIZED(10);
EXPAND_PLATFORM_CURRENT_RAW_TICKS_SYNCHRONIZED(11);
EXPAND_PLATFORM_CURRENT_RAW_TICKS_SYNCHRONIZED(12);
EXPAND_PLATFORM_CURRENT_RAW_TICKS_SYNCHRONIZED(13);
EXPAND_PLATFORM_CURRENT_RAW_TICKS_SYNCHRONIZED(14);
EXPAND_PLATFORM_CURRENT_RAW_TICKS_SYNCHRONIZED(15);
#undef EXPAND_PLATFORM_CURRENT_RAW_TICKS_SYNCHRONIZED

zx_ticks_t platform_convert_early_ticks(arch::EarlyTicks sample) {
  // Early tick timestamps are always raw ticks.  We need to convert back to
  // ticks by subtracting the raw_ticks to ticks offset.
  return sample.*reg_procs.early_ticks + timer_get_mono_ticks_offset();
}

zx_status_t platform_set_oneshot_timer(zx_ticks_t deadline) {
  DEBUG_ASSERT(arch_ints_disabled());

  if (deadline < 0) {
    deadline = 0;
  }

  // Even if the deadline has already passed, the ARMv8-A timer will fire the
  // interrupt.
  write_cval(deadline);
  write_ctl(1);
  kcounter_add(platform_timer_set_counter, 1);

  return 0;
}

void platform_stop_timer() {
  write_ctl(0);
  kcounter_add(platform_timer_cancel_counter, 1);
}

void platform_shutdown_timer() {
  DEBUG_ASSERT(arch_ints_disabled());
  mask_interrupt(timer_irq);
}

bool platform_usermode_can_access_tick_registers() {
  // We always use the ARM generic timer for the tick counter, and these
  // registers are accessible from usermode
  return true;
}

template <bool AllowDebugPrint = false>
static inline affine::Ratio arm_generic_timer_compute_conversion_factors(uint32_t cntfrq) {
  affine::Ratio cntpct_to_nsec = {ZX_SEC(1), cntfrq};
  if constexpr (AllowDebugPrint) {
    dprintf(SPEW, "arm generic timer cntpct_per_nsec: %u/%u\n", cntpct_to_nsec.numerator(),
            cntpct_to_nsec.denominator());
  }
  return cntpct_to_nsec;
}

// Run once on the boot cpu to decide if we want to start an event stream on each
// cpu and at what rate.
static void event_stream_init(uint32_t cntfrq) {
  if (!gBootOptions->arm64_event_stream_enabled) {
    return;
  }

  // Compute the closest power of two from the timer frequency to get to the target.
  //
  // The mechanism to select the rate of the event counter is to select which bit in the virtual
  // counter it should watch for a transition from 0->1 or 1->0 on. This has the effect of
  // of selecting a power of two to divide the virtual counter by plus one.
  //
  // There's no real out of range value here. Everything gets clamped to a shift value of [0, 15].
  uint shift;
  for (shift = 0; shift <= 14; shift++) {
    // Find a matching shift to the target frequency within range. If the target frequency is too
    // large even for shift 0 then it'll just pick shift 0 because of the <=.
    if (log2_uint_floor(cntfrq >> (shift + 1)) <=
        log2_uint_floor(gBootOptions->arm64_event_stream_freq_hz)) {
      break;
    }
  }
  // If we ran off the end of the for loop 15 is the max shift and is okay
  DEBUG_ASSERT(shift <= 15);

  // Save the computed state
  event_stream_shift = shift;
  event_stream_freq = (cntfrq >> (event_stream_shift + 1));

  dprintf(INFO, "arm generic timer will enable event stream on all cpus: shift %u, %u Hz\n",
          event_stream_shift, event_stream_freq);
}

static void event_stream_enable_percpu() {
  if (!gBootOptions->arm64_event_stream_enabled) {
    return;
  }

  DEBUG_ASSERT(event_stream_shift <= 15);

  // Enable the event stream
  uint64_t cntkctl = __arm_rsr64(TIMER_REG_CNTKCTL);
  // Set the trigger bit (field 7:4)
  cntkctl &= ~(0xfUL << 4);
  cntkctl |= event_stream_shift << 4;  // EVNTI
  // Clear the transition bit to 0
  cntkctl &= ~(1 << 3);  // ENVTDIR
  // Enable the stream
  cntkctl |= (1 << 2);  // EVNTEN
  __arm_wsr64(TIMER_REG_CNTKCTL, cntkctl);

  dprintf(INFO, "arm generic timer cpu-%u: event stream enabled\n", arch_curr_cpu_num());
}

static void arm_generic_timer_init(uint32_t freq_override) {
  if (freq_override == 0) {
    // Read the firmware supplied cntfrq register. Note: it may not be correct
    // in buggy firmware situations, so always provide a mechanism to override it.
    timer_cntfrq = __arm_rsr(TIMER_REG_CNTFRQ);
    LTRACEF("cntfrq: %#08x, %u\n", timer_cntfrq, timer_cntfrq);
  } else {
    timer_cntfrq = freq_override;
  }

  dprintf(INFO, "arm generic timer freq %u Hz\n", timer_cntfrq);

  // No way to reasonably continue. Just hard stop.
  ASSERT(timer_cntfrq != 0);

  timer_set_ticks_to_time_ratio(arm_generic_timer_compute_conversion_factors<true>(timer_cntfrq));

  // Set up the hardware timer irq handler for this vector. Use the permanent irq handler
  // registraion scheme since it is enabled on all cpus and does not need any locking
  // for reentrancy and deregistration purposes.
  zx_status_t status = register_permanent_int_handler(timer_irq, &platform_tick, NULL);
  DEBUG_ASSERT(status == ZX_OK);

  // At this point in time, we expect that the `cntkctl_el1` timer register has
  // the bit which permits EL0 access to the VCT to be set, and perhaps also the
  // bit which allows access to the PCT if that happens to be the time reference
  // we have decided to use.
  //
  // *None* of the other timer HW access bits should be set.  EL0 only gets to
  // look at the counter, and nothing more.
  //
  // ASSERT this now.
  [[maybe_unused]] const uint64_t expected =
      CNTKCTL_EL1_ENABLE_VIRTUAL_COUNTER |
      (ArmUsePhysTimerInVdso() ? CNTKCTL_EL1_ENABLE_PHYSICAL_COUNTER : 0);
  ASSERT_MSG(const uint64_t current = __arm_rsr64(TIMER_REG_CNTKCTL);
             current == expected,
             "CNTKCTL_EL1 register does not match reference counter selection (%016lx != %016lx)",
             current, expected);

  // Determine and compute values for the event stream if requested
  event_stream_init(timer_cntfrq);

  // try to enable the event stream if requested
  event_stream_enable_percpu();

  // enable the IRQ on the boot cpu
  LTRACEF("unmask irq %d on cpu %u\n", timer_irq, arch_curr_cpu_num());
  unmask_interrupt(timer_irq);
}

static void arm_generic_timer_init_secondary_cpu(uint level) {
  // try to enable the event stream if requested
  event_stream_enable_percpu();

  LTRACEF("unmask irq %d on cpu %u\n", timer_irq, arch_curr_cpu_num());
  unmask_interrupt(timer_irq);
}

// secondary cpu initialize the timer just before the kernel starts with interrupts enabled
LK_INIT_HOOK_FLAGS(arm_generic_timer_init_secondary_cpu, arm_generic_timer_init_secondary_cpu,
                   LK_INIT_LEVEL_THREADING - 1, LK_INIT_FLAG_SECONDARY_CPUS)

void ArmGenericTimerInit(const zbi_dcfg_arm_generic_timer_driver_t& config) {
  uint32_t irq_phys = config.irq_phys;
  uint32_t irq_virt = config.irq_virt;
  uint32_t irq_sphys = config.irq_sphys;

  // If boot-options have been configured to force us to use the physical
  // counter as our reference, clear out any virtual timer hardware and let
  // nature take its course.  If we don't have an interrupt configured for using
  // the physical timer hardware (either PHYS or SPHYS), we are going to end up
  // panicking.
  if (gBootOptions->arm64_force_pct) {
    dprintf(INFO,
            "arm generic timer forcing use of PCT.  IRQs provided were "
            "(virt %u, phys %u, sphys %u)\n",
            irq_virt, irq_phys, irq_sphys);
    irq_virt = 0;
  }

  // Always prefer to use the virtual timer if we have the option to do so.
  // Additionally, always start by using the versions of the timer read which
  // have the A73 errata workaround.
  //
  // Currently, we have not had a chance to boot all of our CPUs and determine
  // if we have any A73's in the mix.  Until we know, it is safe to use the A73
  // versions of the reads, just a small bit slower.  Later on, if we know it is
  // safe to do so, we can switch to using the workaround-free version.
  const char* timer_str = "";
  if (irq_virt) {
    timer_str = "virt";
    timer_irq = irq_virt;
    timer_assignment = IRQ_VIRT;
    reg_procs = cntv_procs;
    read_arm_counter.store(read_cntvct_a73, ktl::memory_order_relaxed);
  } else if (irq_phys) {
    timer_str = "phys";
    timer_irq = irq_phys;
    timer_assignment = IRQ_PHYS;
    reg_procs = cntp_procs;
    read_arm_counter.store(read_cntpct_a73, ktl::memory_order_relaxed);
    arm64_allow_pct_in_el0();
  } else if (irq_sphys) {
    timer_str = "sphys";
    timer_irq = irq_sphys;
    timer_assignment = IRQ_SPHYS;
    reg_procs = cntps_procs;
    read_arm_counter.store(read_cntpct_a73, ktl::memory_order_relaxed);
    arm64_allow_pct_in_el0();
  } else {
    panic("no irqs set in arm_generic_timer_pdev_init\n");
  }
  ZX_ASSERT(reg_procs.early_ticks);

  // We cannot actually reset the value on the ticks timer, so instead we use
  // the time of clock selection (now) to define the zero point on our ticks
  // timeline moving forward.
  timer_set_initial_ticks_offset(-read_arm_counter.load(ktl::memory_order_relaxed)());
  arch::ThreadMemoryBarrier();

  dprintf(INFO, "arm generic timer using %s timer, irq %d\n", timer_str, timer_irq);

  arm_generic_timer_init(config.freq_override);
}

bool ArmUsePhysTimerInVdso() { return timer_assignment != IRQ_VIRT; }

static void late_update_keep_or_disable_a73_timer_workaround(uint) {
  // By the time we make it to LK_INIT_LEVEL_SMP_READY we should have started
  // and sync'ed up with all of our secondary CPUs. If not, something has gone
  // terribly wrong, and we should continue to use the A73 workaround out of an
  // abundance of caution.
  //
  zx_status_t status = mp_wait_for_all_cpus_ready(Deadline::no_slack(0));
  if (status != ZX_OK) {
    dprintf(ALWAYS,
            "At least one CPU has failed to check in by INIT_LEVEL_SMP_READY in the "
            "init sequence.  Keeping A73 counter workarounds in place.\n");
  } else {
    if (arch_quirks_needs_arm_erratum_858921_mitigation() == false) {
      // If all of our CPUs have checked in, and we have not discovered any A73
      // CPUs, we can switch to using the simple register read instead of the
      // double-read required by the A73 workaround.
      ReadArmCounterFunc& thunk = ArmUsePhysTimerInVdso() ? read_cntpct : read_cntvct;
      read_arm_counter.store(thunk, ktl::memory_order_relaxed);
    } else {
      dprintf(INFO, "A73 cores detected.  Keeping arm generic timer A73 workaround\n");
    }
  }
}

LK_INIT_HOOK(late_update_keep_or_disable_a73_timer_workaround,
             &late_update_keep_or_disable_a73_timer_workaround, LK_INIT_LEVEL_SMP_READY)

/********************************************************************************
 *
 * Tests
 *
 ********************************************************************************/

namespace {

[[maybe_unused]] constexpr uint32_t kMinTestFreq = 1;
[[maybe_unused]] constexpr uint32_t kMaxTestFreq = ktl::numeric_limits<uint32_t>::max();
[[maybe_unused]] constexpr uint32_t kCurTestFreq = 0;

inline uint64_t abs_int64(int64_t a) { return (a > 0) ? a : -a; }

bool test_time_conversion_check_result(uint64_t a, uint64_t b, uint64_t limit) {
  BEGIN_TEST;

  if (a != b) {
    uint64_t diff = abs_int64(a - b);
    ASSERT_LE(diff, limit);
  }

  END_TEST;
}

bool test_time_to_ticks(uint32_t cntfrq) {
  BEGIN_TEST;

  affine::Ratio time_to_ticks;
  if (cntfrq == kCurTestFreq) {
    uint64_t tps = ticks_per_second();
    ASSERT_LE(tps, ktl::numeric_limits<uint32_t>::max());
    cntfrq = static_cast<uint32_t>(tps);
    time_to_ticks = timer_get_ticks_to_time_ratio().Inverse();
  } else {
    time_to_ticks = arm_generic_timer_compute_conversion_factors(cntfrq).Inverse();
  }

  constexpr uint64_t VECTORS[] = {
      0,
      1,
      60 * 60 * 24,
      60 * 60 * 24 * 365,
      60 * 60 * 24 * (365 * 10 + 2),
      60ULL * 60 * 24 * (365 * 100 + 2),
  };

  for (auto vec : VECTORS) {
    uint64_t cntpct = time_to_ticks.Scale(vec);
    constexpr uint32_t nanos_per_sec = ZX_SEC(1);
    uint64_t expected_cntpct = ((uint64_t)cntfrq * vec + (nanos_per_sec / 2)) / nanos_per_sec;

    if (!test_time_conversion_check_result(cntpct, expected_cntpct, 1)) {
      printf("FAIL: zx_time_to_ticks(%" PRIu64 "): got %" PRIu64 ", expect %" PRIu64 "\n", vec,
             cntpct, expected_cntpct);
      ASSERT_TRUE(false);
    }
  }

  END_TEST;
}

bool test_ticks_to_time(uint32_t cntfrq) {
  BEGIN_TEST;

  affine::Ratio ticks_to_time;
  if (cntfrq == kCurTestFreq) {
    uint64_t tps = ticks_per_second();
    ASSERT_LE(tps, ktl::numeric_limits<uint32_t>::max());
    cntfrq = static_cast<uint32_t>(tps);
    ticks_to_time = timer_get_ticks_to_time_ratio();
  } else {
    ticks_to_time = arm_generic_timer_compute_conversion_factors(cntfrq);
  }

  constexpr uint64_t VECTORS[] = {
      1,
      60 * 60 * 24,
      60 * 60 * 24 * 365,
      60 * 60 * 24 * (365 * 10 + 2),
      60ULL * 60 * 24 * (365 * 50 + 2),
  };

  for (auto vec : VECTORS) {
    zx_time_t expected_zx_time = ZX_SEC(vec);
    uint64_t cntpct = (uint64_t)cntfrq * vec;
    zx_time_t zx_time = ticks_to_time.Scale(cntpct);

    const uint64_t limit = (1000 * 1000 + cntfrq - 1) / cntfrq;
    if (!test_time_conversion_check_result(zx_time, expected_zx_time, limit)) {
      printf("ticks_to_zx_time(0x%" PRIx64 "): got 0x%" PRIx64 ", expect 0x%" PRIx64 "\n", cntpct,
             static_cast<uint64_t>(zx_time), static_cast<uint64_t>(expected_zx_time));
      ASSERT_TRUE(false);
    }
  }

  END_TEST;
}

// Verify that the event stream will break CPUs out of WFE.
//
// Start one thread for each CPU that's online and active.  Each thread will then disable
// interrupts and issue a series of WFEs.  If the event stream is working as expected, each thread
// will eventually complete its series of WFEs and terminate.  If the event stream is not working
// as expected, one or more threads will hang.
bool test_event_stream() {
  BEGIN_TEST;

  if (!gBootOptions->arm64_event_stream_enabled) {
    printf("event stream disabled, skipping test\n");
    END_TEST;
  }

  struct Args {
    ktl::atomic<uint32_t> waiting{0};
  };

  auto func = [](void* args_) -> int {
    auto* args = reinterpret_cast<Args*>(args_);
    {
      InterruptDisableGuard guard;

      // Signal that we are ready.
      args->waiting.fetch_sub(1);
      // Wait until everyone else is ready.
      while (args->waiting.load() > 0) {
      }

      // If the event stream is working, it (or something else) will break us out on each iteration.
      for (int i = 0; i < 1000; ++i) {
        // The SEVL sets the event flag for this CPU.  The first WFE consumes the now set event
        // flag.  By setting then consuming, we can be sure the second WFE will actually wait for an
        // event.
        __asm__ volatile("sevl;wfe;wfe");
      }
    }
    printf("cpu-%u done\n", arch_curr_cpu_num());
    return 0;
  };

  Args args;
  Thread* threads[SMP_MAX_CPUS]{};

  // How many online+active CPUs do we have?
  uint32_t num_cpus = ktl::popcount(mp_get_online_mask() & Scheduler::PeekActiveMask());
  args.waiting.store(num_cpus);

  // Create a thread bound to each online+active CPU, but don't start them just yet.
  cpu_num_t last = 0;
  for (cpu_num_t i = 0; i < percpu::processor_count(); ++i) {
    if (mp_is_cpu_online(i) && Scheduler::PeekIsActive(i)) {
      threads[i] = Thread::Create("test_event_stream", func, &args, DEFAULT_PRIORITY);
      threads[i]->SetCpuAffinity(cpu_num_to_mask(i));
      last = i;
    }
  }

  // Because these threads have hard affinity and will disable interrupts we need to take care in
  // how we start them.  If we start one that's bound to our current CPU, we may get preempted
  // deadlock.  To avoid this, bind the current thread to the *last* online+active CPU.
  const cpu_mask_t orig_mask = Thread::Current::Get()->GetCpuAffinity();
  Thread::Current::Get()->SetCpuAffinity(cpu_num_to_mask(last));
  auto restore_mask =
      fit::defer([&orig_mask]() { Thread::Current::Get()->SetCpuAffinity(orig_mask); });

  // Now that we're running on the last online+active CPU we can simply start them in order.
  for (cpu_num_t i = 0; i < percpu::processor_count(); ++i) {
    if (threads[i] != nullptr) {
      threads[i]->Resume();
    }
  }

  // Finally, wait for them to complete.
  for (size_t i = 0; i < percpu::processor_count(); ++i) {
    if (threads[i] != nullptr) {
      threads[i]->Join(nullptr, ZX_TIME_INFINITE);
    }
  }

  END_TEST;
}

}  // namespace

UNITTEST_START_TESTCASE(arm_clock_tests)
UNITTEST("Time --> Ticks (min freq)", []() -> bool { return test_time_to_ticks(kMinTestFreq); })
UNITTEST("Time --> Ticks (max freq)", []() -> bool { return test_time_to_ticks(kMaxTestFreq); })
UNITTEST("Time --> Ticks (cur freq)", []() -> bool { return test_time_to_ticks(kCurTestFreq); })
UNITTEST("Ticks --> Time (min freq)", []() -> bool { return test_ticks_to_time(kMinTestFreq); })
UNITTEST("Ticks --> Time (max freq)", []() -> bool { return test_ticks_to_time(kMaxTestFreq); })
UNITTEST("Ticks --> Time (cur freq)", []() -> bool { return test_ticks_to_time(kCurTestFreq); })
UNITTEST("Event Stream", test_event_stream)
UNITTEST_END_TESTCASE(arm_clock_tests, "arm_clock", "Tests for ARM tick count and current time")
