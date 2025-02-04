// Copyright 2016 The Fuchsia Authors
// Copyright (c) 2014-2016 Travis Geiselbrecht
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <arch.h>
#include <assert.h>
#include <bits.h>
#include <debug.h>
#include <inttypes.h>
#include <lib/arch/arm64/smccc.h>
#include <lib/arch/arm64/system.h>
#include <lib/arch/intrin.h>
#include <lib/arch/sysreg.h>
#include <lib/boot-options/arm64.h>
#include <lib/boot-options/boot-options.h>
#include <lib/console.h>
#include <platform.h>
#include <string.h>
#include <trace.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <arch/arm64.h>
#include <arch/arm64/feature.h>
#include <arch/arm64/mmu.h>
#include <arch/arm64/registers.h>
#include <arch/arm64/uarch.h>
#include <arch/interrupt.h>
#include <arch/mp.h>
#include <arch/ops.h>
#include <arch/regs.h>
#include <arch/vm.h>
#include <kernel/cpu.h>
#include <kernel/mp.h>
#include <kernel/thread.h>
#include <ktl/atomic.h>
#include <ktl/bit.h>
#include <lk/init.h>
#include <lk/main.h>
#include <phys/handoff.h>

#include "smccc.h"

#include <ktl/enforce.h>

#define LOCAL_TRACE 0

namespace {

Arm64AlternateVbar gAlternateVbar;

}  // namespace

// Performance Monitors Count Enable Set, EL0.
static constexpr uint64_t PMCNTENSET_EL0_ENABLE = 1UL << 31;  // Enable cycle count register.

// Performance Monitor Control Register, EL0.
static constexpr uint64_t PMCR_EL0_ENABLE_BIT = 1 << 0;
static constexpr uint64_t PMCR_EL0_LONG_COUNTER_BIT = 1 << 6;

// Performance Monitors User Enable Regiser, EL0.
static constexpr uint64_t PMUSERENR_EL0_ENABLE = 1 << 0;  // Enable EL0 access to cycle counter.

struct arm64_sp_info_t {
  uint64_t mpid;
  void* sp;                   // Stack pointer points to arbitrary data.
  uintptr_t* shadow_call_sp;  // SCS pointer points to array of addresses.

  // This part of the struct itself will serve temporarily as the
  // fake arch_thread in the thread pointer, so that safe-stack
  // and stack-protector code can work early.  The thread pointer
  // (TPIDR_EL1) points just past arm64_sp_info_t.
  uintptr_t stack_guard;
  void* unsafe_sp;
};

static_assert(sizeof(arm64_sp_info_t) == 40, "check arm64_get_secondary_sp assembly");
static_assert(offsetof(arm64_sp_info_t, sp) == 8, "check arm64_get_secondary_sp assembly");
static_assert(offsetof(arm64_sp_info_t, mpid) == 0, "check arm64_get_secondary_sp assembly");

#define TP_OFFSET(field) ((int)offsetof(arm64_sp_info_t, field) - (int)sizeof(arm64_sp_info_t))
static_assert(TP_OFFSET(stack_guard) == ZX_TLS_STACK_GUARD_OFFSET, "");
static_assert(TP_OFFSET(unsafe_sp) == ZX_TLS_UNSAFE_SP_OFFSET, "");
#undef TP_OFFSET

// Used to hold up the boot sequence on secondary CPUs until signaled by the primary.
static ktl::atomic<bool> secondaries_released;

// Whether or not to allow access to the PCT (physical counter) from EL0, in
// addition to allowing access to the VCT (virtual counter).  This decision
// needs to be programmed into each CPU's copy of the CNTKCTL_EL1 register
// during initialization.  By default, we deny access to the PCT and allow
// access to the VCT, but if we determine that we _have_ to use PCT during clock
// selection, we will come back and change this.  Clock selection happens before
// the secondaries have started, so if we change our minds, we only need to
// re-program the boot CPU's register and set this flag.  The secondaries will
// Do The Right Thing during their early init.
//
// Note: this variable is atomic and accessed with relaxed semantics, but it may
// not even need to be that.  Counter selection (the only time this variable is
// mutated) on ARM happens before the secondary CPUs are started and perform
// their early init (the only time they will read this variable).  There should
// be no real chance of a data race here.
static ktl::atomic<bool> allow_pct_in_el0{false};

static void SetupCntkctlEl1() {
  // If the process of clock reference selection has forced us to use the
  // physical counter as our reference, make sure we give EL0 permission to
  // access it.  For now, we still allow access to the virtual counter because
  // there exists some code out there which actually tries to read the VCT in
  // user-mode directly.
  //
  // If/when this eventually changes, we should come back here and lock out
  // access to the VCT when we decide to use the PCT.
  static constexpr uint64_t CNTKCTL_EL1_ENABLE_PHYSICAL_COUNTER = 1 << 0;
  static constexpr uint64_t CNTKCTL_EL1_ENABLE_VIRTUAL_COUNTER = 1 << 1;
  const uint64_t val = CNTKCTL_EL1_ENABLE_VIRTUAL_COUNTER |
                       (allow_pct_in_el0.load() ? CNTKCTL_EL1_ENABLE_PHYSICAL_COUNTER : 0);
  __arm_wsr64("cntkctl_el1", val);
  __isb(ARM_MB_SY);
}

static volatile int secondaries_to_init = 0;

// one for each secondary CPU, indexed by (cpu_num - 1).
static Thread _init_thread[SMP_MAX_CPUS - 1];

// one for each secondary CPU, indexed by (cpu_num - 1).
arm64_sp_info_t arm64_secondary_sp_list[SMP_MAX_CPUS - 1];

zx_status_t arm64_create_secondary_stack(cpu_num_t cpu_num, uint64_t mpid) {
  // Allocate a stack, indexed by CPU num so that |arm64_secondary_entry| can find it.
  DEBUG_ASSERT_MSG(cpu_num > 0 && cpu_num < SMP_MAX_CPUS, "cpu_num: %u", cpu_num);
  KernelStack* stack = &_init_thread[cpu_num - 1].stack();
  DEBUG_ASSERT(stack->base() == 0);
  zx_status_t status = stack->Init();
  if (status != ZX_OK) {
    return status;
  }

  // Get the stack pointers.
  void* sp = reinterpret_cast<void*>(stack->top());
  void* unsafe_sp = nullptr;
  uintptr_t* shadow_call_sp = nullptr;
#if __has_feature(safe_stack)
  DEBUG_ASSERT(stack->unsafe_base() != 0);
  unsafe_sp = reinterpret_cast<void*>(stack->unsafe_top());
#endif
#if __has_feature(shadow_call_stack)
  DEBUG_ASSERT(stack->shadow_call_base() != 0);
  // The shadow call stack grows up.
  shadow_call_sp = reinterpret_cast<uintptr_t*>(stack->shadow_call_base());
#endif

  // Store it.
  LTRACEF("set mpid 0x%lx sp to %p\n", mpid, sp);
#if __has_feature(safe_stack)
  LTRACEF("set mpid 0x%lx unsafe-sp to %p\n", mpid, unsafe_sp);
#endif
#if __has_feature(shadow_call_stack)
  LTRACEF("set mpid 0x%lx shadow-call-sp to %p\n", mpid, shadow_call_sp);
#endif
  arm64_secondary_sp_list[cpu_num - 1].mpid = mpid;
  arm64_secondary_sp_list[cpu_num - 1].sp = sp;
  arm64_secondary_sp_list[cpu_num - 1].stack_guard = Thread::Current::Get()->arch().stack_guard;
  arm64_secondary_sp_list[cpu_num - 1].unsafe_sp = unsafe_sp;
  arm64_secondary_sp_list[cpu_num - 1].shadow_call_sp = shadow_call_sp;

  return ZX_OK;
}

zx_status_t arm64_free_secondary_stack(cpu_num_t cpu_num) {
  DEBUG_ASSERT(cpu_num > 0 && cpu_num < SMP_MAX_CPUS);
  return _init_thread[cpu_num - 1].stack().Teardown();
}

static VbarFunction* arm64_select_vbar_via_smccc11(arch::ArmSmcccFunction function) {
  constexpr auto no_workaround = []() -> VbarFunction* {
    // No mitigation is needed on this CPU.
    WRITE_PERCPU_FIELD(should_invalidate_bp_on_el0_exception, false);
    WRITE_PERCPU_FIELD(should_invalidate_bp_on_context_switch, false);
    return nullptr;
  };

  constexpr auto use_workaround = []() -> VbarFunction* {
    // The workaround replaces the other EL0 entry mitigations.
    WRITE_PERCPU_FIELD(should_invalidate_bp_on_el0_exception, false);

    // The EL0->EL1 entry mitigation is sufficient without the context-switch
    // mitigation too.
    WRITE_PERCPU_FIELD(should_invalidate_bp_on_context_switch, false);

    return arm64_el1_exception_smccc11_workaround;
  };

  const unsigned int cpu_num = arch_curr_cpu_num();

  if (gBootOptions->arm64_alternate_vbar != Arm64AlternateVbar::kAuto) {
    dprintf(INFO,
            "CPU %u using SMCCC_ARCH_WORKAROUND function %#" PRIx32 " by boot option override\n",
            cpu_num, static_cast<uint32_t>(function));
    return use_workaround();
  }

  // The workaround call is supported by the firmware on all CPUs.
  // Check on each individual CPU whether it needs to be used or not.
  uint64_t value =
      ArmSmcccCall(arch::ArmSmcccFunction::kSmcccArchFeatures, static_cast<uint32_t>(function));

  switch (value) {
    case 0:
      dprintf(INFO, "CPU %u firmware requires SMCCC_ARCH_WORKAROUND function %#" PRIx32 "\n",
              cpu_num, static_cast<uint32_t>(function));
      return use_workaround();

    case 1:
      dprintf(INFO,
              "CPU %u firmware reports SMCCC_ARCH_WORKAROUND function %#" PRIx32 " not needed\n",
              cpu_num, static_cast<uint32_t>(function));
      return no_workaround();

    default:
      dprintf(CRITICAL,
              "WARNING: Possible SMCCC firmware bug: "
              " SMCCC_ARCH_FEATURES reports %" PRId64 " for %#" PRIx32
              " on CPU %u but boot CPU reported it supported!\n",
              ktl::bit_cast<int64_t>(value), static_cast<uint32_t>(function), cpu_num);
  }

  return nullptr;
}

// Select the alternate exception vector to use for the current CPU.
// Returns nullptr to keep using the default one.
static VbarFunction* arm64_select_vbar() {
  // In auto mode, the physboot detection code has "selected" a firmware option
  // if it's available generally.  The logic here then chooses whether this
  // particular CPU needs to use that firmware option by asking the firmware.
  switch (gAlternateVbar) {
    case Arm64AlternateVbar::kArchWorkaround3:
      return arm64_select_vbar_via_smccc11(arch::ArmSmcccFunction::kSmcccArchWorkaround3);
    case Arm64AlternateVbar::kArchWorkaround1:
      return arm64_select_vbar_via_smccc11(arch::ArmSmcccFunction::kSmcccArchWorkaround1);
    case Arm64AlternateVbar::kPsciVersion:
      // TODO(https://fxbug.dev/322202704): Auto-select based on core IDs?
      dprintf(INFO, "CPU %u using SMCCC 1.1 PSCI_VERSION in lieu of SMCCC_ARCH_WORKAROUND\n",
              arch_curr_cpu_num());
      return arm64_el1_exception_smccc11_workaround;
    case Arm64AlternateVbar::kSmccc10:
      // TODO(https://fxbug.dev/322202704): Auto-select based on core IDs?
      dprintf(INFO, "CPU %u using SMCCC 1.0 PSCI_VERSION in lieu of SMCCC 1.1 support\n",
              arch_curr_cpu_num());
      return arm64_el1_exception_smccc10_workaround;
    case Arm64AlternateVbar::kNone:
      if (gBootOptions->arm64_alternate_vbar == Arm64AlternateVbar::kNone) {
        dprintf(INFO, "CPU %u not using any workaround by explicit boot option\n",
                arch_curr_cpu_num());
        break;
      }
      ZX_ASSERT(gBootOptions->arm64_alternate_vbar == Arm64AlternateVbar::kAuto);
      // TODO(https://fxbug.dev/322202704): fall back to branch loop?
      // Just panic on known cores with issues when firmware is lacking?
      dprintf(INFO, "CPU %u has no SMCCC workaround function configured\n", arch_curr_cpu_num());
      break;
    case Arm64AlternateVbar::kAuto:
      ZX_PANIC("physboot handoff should have performed auto-selection!");
      break;
  }
  return nullptr;
}

// Set the vector base.
static void arm64_install_vbar(VbarFunction* table) {
  arch::ArmVbarEl1::Write(reinterpret_cast<uintptr_t>(table));
  __isb(ARM_MB_SY);
}

static void arm64_cpu_early_init() {
  // Make sure the per cpu pointer is set up.
  arm64_init_percpu_early();

  // Initially use the primary vector table.
  // arch_late_init_percpu may change its mind.
  arm64_install_vbar(arm64_el1_exception);

  // Set up main control bits for this cpu.
  auto sctlr = arch::ArmSctlrEl1::Get().FromValue(0);
  sctlr
      .set_uci(true)   // Do not trap DC cache instructions in EL0.
      .set_span(true)  // Do not change PSTATE.PAN on exception.
      .set_ntwe(true)  // Do not trap WFE in EL0.
      .set_uct(true)   // Do not trap CTR_EL0 in EL0
      .set_dze(true)   // Do not trap DZ ZVA in EL0.
      .set_i(true)     // Instruction cache enable.
      .set_sa0(true)   // Stack pointer alignment in EL0.
      .set_sa(true)    // Stack pointer alignment in EL1.
      .set_c(true)     // Data cache enable.
      .set_m(true);    // MMU Enable.
  arch::ArmSctlrEl1::Write(sctlr);
  __isb(ARM_MB_SY);

  // Hard disable the FPU, SVE, and any additional vector units.
  __arm_wsr64("cpacr_el1", 0);
  __isb(ARM_MB_SY);

  // Save all of the features of the cpu.
  arm64_feature_init();

  // Check for TCR2 and SCTLR2 and zero since none of their features are used.
  auto mmfr3 = arch::ArmIdAa64Mmfr3El1::Read();
  if (mmfr3.tcrx() != 0) {
    auto tcr2 = arch::ArmTcr2El1::Get().FromValue(0);
    arch::ArmTcr2El1::Write(tcr2);
    __isb(ARM_MB_SY);
  }
  if (mmfr3.sctlrx() != 0) {
    auto sctlr2 = arch::ArmSctlr2El1::Get().FromValue(0);
    arch::ArmSctlr2El1::Write(sctlr2);
    __isb(ARM_MB_SY);
  }

  // Enable cycle counter, if FEAT_PMUv3 is enabled.
  if (feat_pmuv3_enabled) {
    __arm_wsr64("pmcr_el0", PMCR_EL0_ENABLE_BIT | PMCR_EL0_LONG_COUNTER_BIT);
    __isb(ARM_MB_SY);
    __arm_wsr64("pmcntenset_el0", PMCNTENSET_EL0_ENABLE);
    __isb(ARM_MB_SY);

    // Enable user space access to cycle counter.
    __arm_wsr64("pmuserenr_el0", PMUSERENR_EL0_ENABLE);
    __isb(ARM_MB_SY);
  }

  // Enable Debug Exceptions by Disabling the OS Lock. The OSLAR_EL1 is a WO
  // register with only the low bit defined as OSLK. Write 0 to disable.
  __arm_wsr64("oslar_el1", 0x0);
  __isb(ARM_MB_SY);

  // Give EL0 access to the chosen reference counter, but nothing else.
  SetupCntkctlEl1();

  __arm_wsr64("mdscr_el1", MSDCR_EL1_INITIAL_VALUE);
  __isb(ARM_MB_SY);

  arch_enable_fiqs();
}

void arch_early_init() {
  // Collect the setting that physboot determined.  arch_late_init_percpu()
  // will call arm64_select_vbar to use it later, when gPhysHandoff may no
  // longer be available.
  DEBUG_ASSERT(gPhysHandoff != nullptr);
  gAlternateVbar = gPhysHandoff->arch_handoff.alternate_vbar;

  // put the cpu in a working state and read the feature flags
  arm64_cpu_early_init();
}

void arch_prevm_init() { arm64_boot_mmu_unwire(); }

void arch_init() TA_NO_THREAD_SAFETY_ANALYSIS {
  arch_mp_init_percpu();

  dprintf(INFO, "ARM boot EL%lu\n", arm64_get_boot_el());
  auto [total_boot_mem, used_boot_mem] = arm64_boot_map_used_memory();
  dprintf(INFO, "ARM used %#zx bytes out of %#zx bytes for boot page tables\n", used_boot_mem,
          total_boot_mem);

  arm64_feature_debug(true);

  uint32_t max_cpus = arch_max_num_cpus();
  uint32_t cmdline_max_cpus = gBootOptions->smp_max_cpus;
  if (cmdline_max_cpus > max_cpus || cmdline_max_cpus <= 0) {
    printf("invalid kernel.smp.maxcpus value, defaulting to %u\n", max_cpus);
    cmdline_max_cpus = max_cpus;
  }

  secondaries_to_init = cmdline_max_cpus - 1;

  lk_init_secondary_cpus(secondaries_to_init);

  LTRACEF("releasing %d secondary cpus\n", secondaries_to_init);
  secondaries_released.store(true);

  // Flush the signaling variable since the secondary cpus may have not yet enabled their caches.
  arch_clean_cache_range((vaddr_t)&secondaries_released, sizeof(secondaries_released));
}

void arch_late_init_percpu(void) {
  const bool need_spectre_v2_mitigation =
      !gBootOptions->arm64_disable_spec_mitigations && arm64_uarch_needs_spectre_v2_mitigation();

  // These may be reset in arm64_select_vbar() when something better is chosen.
  WRITE_PERCPU_FIELD(should_invalidate_bp_on_context_switch, need_spectre_v2_mitigation);
  WRITE_PERCPU_FIELD(should_invalidate_bp_on_el0_exception, need_spectre_v2_mitigation);

  // Decide if this CPU needs an alternative exception vector table.
  if (VbarFunction* vector_table = arm64_select_vbar()) {
    arm64_install_vbar(vector_table);
  }
}

void ArchIdlePowerThread::EnterIdleState(zx_duration_t max_latency) { __asm__ volatile("wfi"); }

void arch_setup_uspace_iframe(iframe_t* iframe, uintptr_t pc, uintptr_t sp, uintptr_t arg1,
                              uintptr_t arg2) {
  // Set up a default spsr to get into 64bit user space:
  //  - Zeroed NZCV.
  //  - No SS, no IL, no D.
  //  - All interrupts enabled.
  //  - Mode 0: EL0t.
  uint32_t spsr = 0;

  iframe->r[0] = arg1;
  iframe->r[1] = arg2;
  iframe->usp = sp;
  iframe->elr = pc;
  iframe->spsr = spsr;
}

// Switch to user mode, set the user stack pointer to user_stack_top, put the svc stack pointer to
// the top of the kernel stack.
void arch_enter_uspace(iframe_t* iframe) {
  DEBUG_ASSERT(arch_ints_disabled());

  Thread* ct = Thread::Current::Get();

  LTRACEF("r0 %#" PRIxPTR " r1 %#" PRIxPTR " spsr %#" PRIxPTR " st %#" PRIxPTR " usp %#" PRIxPTR
          " pc %#" PRIxPTR "\n",
          iframe->r[0], iframe->r[1], iframe->spsr, ct->stack().top(), iframe->usp, iframe->elr);
#if __has_feature(shadow_call_stack)
  auto scsp_base = ct->stack().shadow_call_base();
  LTRACEF("scsp %p, scsp base %#" PRIxPTR "\n", ct->arch().shadow_call_sp, scsp_base);
#endif

  ASSERT(arch_is_valid_user_pc(iframe->elr));

#if __has_feature(shadow_call_stack)
  arm64_uspace_entry(iframe, ct->stack().top(), scsp_base);
#else
  arm64_uspace_entry(iframe, ct->stack().top());
#endif
  __UNREACHABLE;
}

void arm64_allow_pct_in_el0() {
  ASSERT(secondaries_released.load() == false);
  allow_pct_in_el0.store(true, ktl::memory_order_relaxed);
  SetupCntkctlEl1();
}

// called from assembly.
extern "C" void arm64_secondary_entry();

extern "C" void arm64_secondary_entry() {
  arm64_cpu_early_init();

  // Wait until the primary has finished setting things up.
  while (!secondaries_released.load()) {
    arch::Yield();
  }

  cpu_num_t cpu = arch_curr_cpu_num();
  _init_thread[cpu - 1].SecondaryCpuInitEarly();
  // Run early secondary cpu init routines up to the threading level.
  lk_init_level(LK_INIT_FLAG_SECONDARY_CPUS, LK_INIT_LEVEL_EARLIEST, LK_INIT_LEVEL_THREADING - 1);

  arch_mp_init_percpu();

  const bool full_dump = arm64_feature_current_is_first_in_cluster();
  arm64_feature_debug(full_dump);

  lk_secondary_cpu_entry();
}

static int cmd_cpu(int argc, const cmd_args* argv, uint32_t flags) {
  auto usage = [cmd_name = argv[0].str]() -> int {
    printf("usage:\n");
    printf("%s sev                              : issue a SEV (Send Event) instruction\n",
           cmd_name);
    return ZX_ERR_INTERNAL;
  };

  if (argc < 2) {
    printf("not enough arguments\n");
    return usage();
  }

  if (!strcmp(argv[1].str, "sev")) {
    __asm__ volatile("sev");
    printf("done\n");
  } else {
    printf("unknown command\n");
    return usage();
  }

  return ZX_OK;
}

STATIC_COMMAND_START
STATIC_COMMAND("cpu", "cpu diagnostic commands", &cmd_cpu)
STATIC_COMMAND_END(cpu)
