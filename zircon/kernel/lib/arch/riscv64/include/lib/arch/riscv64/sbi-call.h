// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_LIB_ARCH_RISCV64_INCLUDE_LIB_ARCH_RISCV64_SBI_CALL_H_
#define ZIRCON_KERNEL_LIB_ARCH_RISCV64_INCLUDE_LIB_ARCH_RISCV64_SBI_CALL_H_

// RISC-V Supervisor Binary Interface Specification
//
// https://github.com/riscv-non-isa/riscv-sbi-doc/blob/master/riscv-sbi.adoc

#include <lib/arch/riscv64/sbi.h>

#include <cstdint>

namespace arch {

// This provides generic arch::SbiCall<EID, FID> calls for any number of
// integer arguments.  Convenience functions using these are defined below in
// the arch::RiscvSbi:: namespace.

template <RiscvSbiEid Eid, auto Fid>
inline RiscvSbiRet SbiCall() {
  RiscvSbiRet result;
  __asm__ volatile(
      "li a7, %[eid]\n"
      "li a6, %[fid]\n"
      "ecall\n"
      "mv %[error], a0\n"
      "mv %[value], a1\n"
      : [error] "=r"(result.error), [value] "=r"(result.value)
      : [eid] "i"(Eid), [fid] "i"(Fid)
      : "memory", "a0", "a1", "a6", "a7");
  return result;
}

template <RiscvSbiEid Eid, auto Fid>
inline RiscvSbiRet SbiCall(uintptr_t a0) {
  RiscvSbiRet result;
  __asm__ volatile(
      "li a7, %[eid]\n"
      "li a6, %[fid]\n"
      "mv a0, %[a0]\n"
      "ecall\n"
      "mv %[error], a0\n"
      "mv %[value], a1\n"
      : [error] "=r"(result.error), [value] "=r"(result.value)
      : [eid] "i"(Eid), [fid] "i"(Fid), [a0] "r"(a0)
      : "memory", "a0", "a1", "a6", "a7");
  return result;
}

template <RiscvSbiEid Eid, auto Fid>
inline RiscvSbiRet SbiCall(uintptr_t a0, uintptr_t a1) {
  RiscvSbiRet result;
  __asm__ volatile(
      "li a7, %[eid]\n"
      "li a6, %[fid]\n"
      "mv a0, %[a0]\n"
      "mv a1, %[a1]\n"
      "ecall\n"
      "mv %[error], a0\n"
      "mv %[value], a1\n"
      : [error] "=r"(result.error), [value] "=r"(result.value)
      : [eid] "i"(Eid), [fid] "i"(Fid), [a0] "r"(a0), [a1] "r"(a1)
      : "memory", "a0", "a1", "a6", "a7");
  return result;
}

template <RiscvSbiEid Eid, auto Fid>
inline RiscvSbiRet SbiCall(uintptr_t a0, uintptr_t a1, uintptr_t a2) {
  RiscvSbiRet result;
  __asm__ volatile(
      "li a7, %[eid]\n"
      "li a6, %[fid]\n"
      "mv a0, %[a0]\n"
      "mv a1, %[a1]\n"
      "mv a2, %[a2]\n"
      "ecall\n"
      "mv %[error], a0\n"
      "mv %[value], a1\n"
      : [error] "=r"(result.error), [value] "=r"(result.value)
      : [eid] "i"(Eid), [fid] "i"(Fid), [a0] "r"(a0), [a1] "r"(a1), [a2] "r"(a2)
      : "memory", "a0", "a1", "a2", "a6", "a7");
  return result;
}

template <RiscvSbiEid Eid, auto Fid>
inline RiscvSbiRet SbiCall(uintptr_t a0, uintptr_t a1, uintptr_t a2, uintptr_t a3) {
  RiscvSbiRet result;
  __asm__ volatile(
      "li a7, %[eid]\n"
      "li a6, %[fid]\n"
      "mv a0, %[a0]\n"
      "mv a1, %[a1]\n"
      "mv a2, %[a2]\n"
      "mv a3, %[a3]\n"
      "ecall\n"
      "mv %[error], a0\n"
      "mv %[value], a1\n"
      : [error] "=r"(result.error), [value] "=r"(result.value)
      : [eid] "i"(Eid), [fid] "i"(Fid), [a0] "r"(a0), [a1] "r"(a1), [a2] "r"(a2), [a3] "r"(a3)
      : "memory", "a0", "a1", "a2", "a3", "a6", "a7");
  return result;
}

template <RiscvSbiEid Eid, auto Fid>
inline RiscvSbiRet SbiCall(uintptr_t a0, uintptr_t a1, uintptr_t a2, uintptr_t a3, uintptr_t a4) {
  RiscvSbiRet result;
  __asm__ volatile(
      "li a7, %[eid]\n"
      "li a6, %[fid]\n"
      "mv a0, %[a0]\n"
      "mv a1, %[a1]\n"
      "mv a2, %[a2]\n"
      "mv a3, %[a3]\n"
      "mv a4, %[a4]\n"
      "ecall\n"
      "mv %[error], a0\n"
      "mv %[value], a1\n"
      : [error] "=r"(result.error), [value] "=r"(result.value)
      : [eid] "i"(Eid), [fid] "i"(Fid), [a0] "r"(a0), [a1] "r"(a1), [a2] "r"(a2), [a3] "r"(a3),
        [a4] "r"(a4)
      : "memory", "a0", "a1", "a2", "a3", "a4", "a6", "a7");
  return result;
}

template <RiscvSbiEid Eid, auto Fid>
inline RiscvSbiRet SbiCall(uintptr_t a0, uintptr_t a1, uintptr_t a2, uintptr_t a3, uintptr_t a4,
                           uintptr_t a5) {
  RiscvSbiRet result;
  __asm__ volatile(
      "li a7, %[eid]\n"
      "li a6, %[fid]\n"
      "mv a0, %[a0]\n"
      "mv a1, %[a1]\n"
      "mv a2, %[a2]\n"
      "mv a3, %[a3]\n"
      "mv a4, %[a4]\n"
      "mv a5, %[a5]\n"
      "ecall\n"
      "mv %[error], a0\n"
      "mv %[value], a1\n"
      : [error] "=r"(result.error), [value] "=r"(result.value)
      : [eid] "i"(Eid), [fid] "i"(Fid), [a0] "r"(a0), [a1] "r"(a1), [a2] "r"(a2), [a3] "r"(a3),
        [a4] "r"(a4), [a5] "r"(a5)
      : "memory", "a0", "a1", "a2", "a3", "a4", "a5", "a6", "a7");
  return result;
}

namespace RiscvSbi {

// Base Extension calls

inline RiscvSbiRet GetSpecVersion() {
  return SbiCall<RiscvSbiEid::kBase, RiscvSbiBase::kGetSpecVersion>();
}

inline RiscvSbiRet GetImplId() { return SbiCall<RiscvSbiEid::kBase, RiscvSbiBase::kGetImplId>(); }

inline RiscvSbiRet GetImplVersion() {
  return SbiCall<RiscvSbiEid::kBase, RiscvSbiBase::kGetImplVersion>();
}

inline RiscvSbiRet ProbeExtension(RiscvSbiEid eid) {
  return SbiCall<RiscvSbiEid::kBase, RiscvSbiBase::kProbeExtension>(static_cast<uintptr_t>(eid));
}

inline RiscvSbiRet GetMvendorid() {
  return SbiCall<RiscvSbiEid::kBase, RiscvSbiBase::kGetMvendorid>();
}

inline RiscvSbiRet GetMarchid() { return SbiCall<RiscvSbiEid::kBase, RiscvSbiBase::kGetMarchid>(); }

inline RiscvSbiRet GetMimpid() { return SbiCall<RiscvSbiEid::kBase, RiscvSbiBase::kGetMimpid>(); }

// IPI Extension calls

inline RiscvSbiRet SendIpi(HartMask hart_mask, HartMaskBase hart_mask_base) {
  return SbiCall<RiscvSbiEid::kIpi, RiscvSbiIpi::kSendIpi>(hart_mask, hart_mask_base);
}

// System Reset Extension calls

inline RiscvSbiRet SystemReset(RiscvSbiResetType reset_type, RiscvSbiResetReason reset_reason) {
  return SbiCall<RiscvSbiEid::kSystemReset, RiscvSbiSystemReset::kSystemReset>(
      static_cast<uint32_t>(reset_type), static_cast<uint32_t>(reset_reason));
}

}  // namespace RiscvSbi

}  // namespace arch

#endif  // ZIRCON_KERNEL_LIB_ARCH_RISCV64_INCLUDE_LIB_ARCH_RISCV64_SBI_CALL_H_
