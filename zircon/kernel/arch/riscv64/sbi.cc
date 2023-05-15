// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT
#include "arch/riscv64/sbi.h"

#include <debug.h>
#include <lib/arch/riscv64/sbi-call.h>
#include <lib/arch/riscv64/sbi.h>

#include <ktl/forward.h>

// Basic SBI wrapper routines and extension detection.
//
// NOTE: Assumes a few extensions are present.
// Timer, Ipi, Rfence, Hart management, and System Reset are all
// assumed to be present. This may need to be revisited in the future
// if it turns out some of these are not always there.
//
// SBI API documentation lives at
// https://github.com/riscv-non-isa/riscv-sbi-doc/blob/master/riscv-sbi.adoc

namespace {
enum class sbi_extension : uint8_t {
  Base,
  Timer,
  Ipi,
  Rfence,
  Hart,
  SystemReset,
  Pmu,
  Dbcn,
  Susp,
  Cppc,
};

// A bitmap of all the detected SBI extensions.
uint32_t supported_extension_bitmap;

// For every extension we track, call a callable with all the information we
// may want.
template <typename Callable>
void for_every_extension(Callable callable) {
  callable(sbi_extension::Base, "BASE", arch::RiscvSbiEid::kBase);
  callable(sbi_extension::Timer, "TIMER", arch::RiscvSbiEid::kTimer);
  callable(sbi_extension::Ipi, "IPI", arch::RiscvSbiEid::kIpi);
  callable(sbi_extension::Rfence, "RFENCE", arch::RiscvSbiEid::kRfence);
  callable(sbi_extension::Hart, "HSM", arch::RiscvSbiEid::kHart);
  callable(sbi_extension::SystemReset, "SRST", arch::RiscvSbiEid::kSystemReset);
  callable(sbi_extension::Pmu, "PMU", arch::RiscvSbiEid::kPmu);
  callable(sbi_extension::Dbcn, "DBCN", arch::RiscvSbiEid::kDbcn);
  callable(sbi_extension::Susp, "SUSP", arch::RiscvSbiEid::kSusp);
  callable(sbi_extension::Cppc, "CPPC", arch::RiscvSbiEid::kCppc);
}

bool sbi_extension_present(sbi_extension ext) {
  return (1U << static_cast<uint8_t>(ext)) & supported_extension_bitmap;
}

}  // anonymous namespace

void riscv64_sbi_early_init() {
  // Probe to see what extensions are present
  auto probe_and_set_extension = [](sbi_extension extension_bit, const char *,
                                    arch::RiscvSbiEid eid) {
    // Base extension is always present
    if (extension_bit == sbi_extension::Base) {
      supported_extension_bitmap = 1U << static_cast<uint8_t>(sbi_extension::Base);
      return;
    }

    // Probe the extension
    arch::RiscvSbiRet ret = arch::RiscvSbi::ProbeExtension(eid);

    // It shouldn't be legal for the base probe extension call to return anything but success,
    // but check here anyway.
    if (ret.error != arch::RiscvSbiError::kSuccess) {
      return;
    }

    supported_extension_bitmap |=
        (ret.value != 0) ? (1U << static_cast<uint8_t>(extension_bit)) : 0;
  };

  for_every_extension(probe_and_set_extension);
}

void riscv64_sbi_init() {
  // Dump SBI version info and extensions found in early probing
  if (DPRINTF_ENABLED_FOR_LEVEL(INFO)) {
    dprintf(INFO, "RISCV: mvendorid %#lx marchid %#lx mimpid %#lx\n",
            arch::RiscvSbi::GetMvendorid().value, arch::RiscvSbi::GetMarchid().value,
            arch::RiscvSbi::GetMimpid().value);

    uint64_t spec_version = arch::RiscvSbi::GetSpecVersion().value;
    dprintf(INFO, "RISCV: SBI spec version %lu.%lu impl id %#lx version %#lx\n",
            (spec_version >> 24) & 0x7f, spec_version & ((1 << 24) - 1),
            arch::RiscvSbi::GetImplId().value, arch::RiscvSbi::GetImplVersion().value);

    dprintf(INFO, "RISCV: extensions: ");
    auto print_extension = [](sbi_extension extension, const char *name, arch::RiscvSbiEid) {
      if (sbi_extension_present(extension)) {
        dprintf(INFO, "%s ", name);
      }
    };
    for_every_extension(print_extension);
    dprintf(INFO, "\n");
  }
}

arch::RiscvSbiRet sbi_send_ipi(arch::HartMask mask, arch::HartMaskBase mask_base) {
  return arch::RiscvSbi::SendIpi(mask, mask_base);
}

arch::RiscvSbiRet sbi_hart_start(arch::HartId hart_id, paddr_t start_addr, uint64_t priv) {
  return arch::RiscvSbi::HartStart(hart_id, start_addr, priv);
}

arch::RiscvSbiRet sbi_remote_sfence_vma(cpu_mask_t cpu_mask, uintptr_t start, uintptr_t size) {
  // TODO: translate cpu mask to hart mask using routine from later commit
  arch::HartMask hart_mask = cpu_mask;
  arch::HartMaskBase hart_mask_base = 0;

  return arch::RiscvSbi::RemoteSfenceVma(hart_mask, hart_mask_base, start, size);
}

arch::RiscvSbiRet sbi_remote_sfence_vma_asid(cpu_mask_t cpu_mask, uintptr_t start, uintptr_t size,
                                             uint64_t asid) {
  // TODO: translate cpu mask to hart mask using routine from later commit
  arch::HartMask hart_mask = cpu_mask;
  arch::HartMaskBase hart_mask_base = 0;

  return arch::RiscvSbi::RemoteSfenceVmaAsid(hart_mask, hart_mask_base, start, size, asid);
}

void sbi_shutdown() {
  arch::RiscvSbi::SystemReset(arch::RiscvSbiResetType::kShutdown, arch::RiscvSbiResetReason::kNone);

  // If we get here the shutdown call must have failed
  panic("SBI: failed to shutdown\n");
}
