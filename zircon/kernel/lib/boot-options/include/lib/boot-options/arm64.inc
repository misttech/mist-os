// Copyright 2021 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

// This file is #include'd multiple times with the DEFINE_OPTION macro defined.
// See kernel/lib/boot-options/README.md for details.
//
// These are the arm64-specific boot options.

DEFINE_OPTION("kernel.arm64.disable_spec_mitigations", bool, arm64_disable_spec_mitigations,
              {false}, R"""(
If set, disables all speculative execution information leak mitigations.

If unset, the per-mitigation defaults will be used.
)""")

DEFINE_OPTION("kernel.arm64.event-stream.enable", bool, arm64_event_stream_enabled, {true}, R"""(
When enabled, each ARM cpu will enable an event stream generator, which per-cpu
sets the hidden event flag at a particular rate. This has the effect of kicking
cpus out of any WFE states they may be sitting in.
)""")

DEFINE_OPTION("kernel.arm64.event-stream.freq-hz", uint32_t, arm64_event_stream_freq_hz, {10000},
              R"""(
If the event stream is enabled, specifies the frequency at which it will attempt
to run. The resolution is limited, so the driver will only be able to pick the
nearest power of 2 from the cpu timer counter.
)""")

DEFINE_OPTION("kernel.arm64.debug.dap-rom-soc", SmallString, arm64_debug_dap_rom_soc, {""}, R"""(
If set, tries to initialize the dap debug aperture at a hard coded address for the particular
system on chip. Currently accepted values are amlogic-t931g, amlogic-s905d2, amlogic-s905d3g, and
amlogic-a311d.
)""")

// TODO(https://fxbug.dev/42160387): Unify arch agnostic options.
DEFINE_OPTION("kernel.smp.maxcpus", uint32_t, arm64_smp_max_cpus, {16}, R"""(
This option caps the number of CPUs to initialize.  It cannot be greater than *SMP\_MAX\_CPUS*
for a specific architecture.

Note: The default value may vary for each architecture.
)""")

DEFINE_OPTION("kernel.phys.psci-reset", Arm64PhysPsciReset, phys_psci_reset,
              {Arm64PhysPsciReset::kReboot}, R"""(
This option determines what kind of PSCI reset operation (if any)
the early boot kernel will use if it needs to panic and crash.
If this is "disabled", the machine may enter an infinite loop on panic.
)""")

DEFINE_OPTION("kernel.arm64.phys.mmu", bool, arm64_phys_mmu, {true}, R"""(
This enables use of the MMU and caches during the kernel's early boot phase.
)""")

DEFINE_OPTION("kernel.arm64.enable-asid", bool, arm64_enable_asid, {true}, R"""(
This enables use of ASIDs. True by default if the underlying hardware supports 16-bit ASIDs.
)""")

DEFINE_OPTION("kernel.arm64.alternate-vbar", Arm64AlternateVbar, arm64_alternate_vbar, {Arm64AlternateVbar::kAuto}, R"""(
This selects the alternate exception vector implementation used to work around
CPU-specific issues on entry to EL1 from EL0.  Values can be:
 * `none` - No mitigations performed in early EL0 exception paths.
 * `auto` (default) - Select SMCCC function identifier based on availability
   reported by firmware via SMCCC / PSCI interfaces. Then each individual CPU
   queries the firmware for whether the workaround is needed at all. If the
   firmware is not SMCCC >= 1.1 that reports it supports the
   SMCCC_ARCH_WORKAROUND_3 and/or SMCCC_ARCH_WORKAROUND_1 function identifiers,
   then this has the same effect as `none`. If the firmware does support it,
   then each CPU either uses the firmware workaround or not, according to what
   the firmware says is needed on that CPU. Either way, alternative spectre
   mitigations, that are either redundant with the firmware workaround or not
   needed if the firmware workaround is not needed, are disabled.
 * `arch3` - Always use SMCCC_ARCH_WORKAROUND_3 on every CPU.  **This may have
   unpredictable effects if the firmware does not support SMCCC 1.1 or does not
   support SMCCC_ARCH_WORKAROUND_3.** This makes every CPU do what `auto` will
   select for each individual CPU that the firmware says should use it, when
   the firmware supports the function. This also disables alternative spectre
   mitigations that are redundant with SMCCC_ARCH_WORKAROUND_3.
 * `arch1` - Always use SMCCC_ARCH_WORKAROUND_1 on every CPU.  **This may have
   unpredictable effects if the firmware does not support SMCCC 1.1 or does not
   support SMCCC_ARCH_WORKAROUND_1.** This makes every CPU do what `auto` will
   select for each individual CPU that the firmware says should use it, when
   the firmware supports the "1" function but not the "3" function. This also
   disables alternative spectre mitigations that are redundant with
   SMCCC_ARCH_WORKAROUND_1.
 * `psci` - Always use PSCI_VERSION with SMCCC 1.1 calling conventions, on
   every CPU.  **This may have unpredictable effects if the firmware does not
   support SMCCC 1.1.** Currently `auto` never selects this behavior for any
   CPU.
 * `smccc10` - Always use PSCI_VERSION with SMCCC 1.0 calling conventions, on
   every CPU.  This is safe on all systems, though has it more overhead than
   SMCCC 1.1 options and may not be necessary on all CPUs.  Currently `auto`
   never selects this behavior for any CPU.
As on SoCs with heterogeneous cores, a different selection should be made for
each individual CPU, values other than `auto` should only be used in testing.
)""")
DEFINE_OPTION("kernel.arm64.force-pct", bool, arm64_force_pct, {false}, R"""(
By default, the kernel will select the ARM virtual counter (VCT) to use as the
time reference for the system.  It will only choose the physical counter (PCT)
if it was given an IRQ for the physical timer hardware, but not the virtual
timer hardware.  When the force-pct option is set, the kernel will _always_
choose to use the PCT as the time reference, even if a VCT IRQ is offered in the
ZBI.  Please note, setting force-pct on a target which has no defined PCT IRQ
will result in a panic during early boot.
)""")

// TODO(https://fxbug.dev/42182308): Remove once user space support is complete
DEFINE_OPTION("kernel.arm64.pci", bool, arm64_kernel_pci, {true}, R"""(
Enables kernel mode PCI support and kernel PCI syscalls.
)""")
