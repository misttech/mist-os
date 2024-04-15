// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/crashlog.h>
#include <lib/fit/defer.h>
#include <platform.h>
#include <stdint.h>
#include <zircon/assert.h>

#include "ubsan-types.h"

// LLVM provides no documentation on the ABI between the compiler and
// the runtime.  The set of function signatures here was culled from
// the LLVM sources for the compiler instrumentation and the runtime.
//
// See
// https://github.com/llvm/llvm-project/tree/eb8ebabfb0efcae69e682b592f12366c3b82e78d/compiler-rt/lib/ubsan

namespace ubsan {
namespace {

auto UbsanPanicStart(const char* check, SourceLocation& loc,
                     void* caller = __builtin_return_address(0),
                     void* frame = __builtin_frame_address(0)) {
  platform_panic_start();
  fprintf(&stdout_panic_buffer,
          "\n"
          "*** KERNEL PANIC (caller pc: %p, stack frame: %p):\n"
          "*** ",
          caller, frame);

  fprintf(&stdout_panic_buffer, "UBSAN CHECK FAILED at (%s:%d): %s\n", loc.filename, loc.line,
          check);

  return fit::defer([]() {
    fprintf(&stdout_panic_buffer, "\n");
    platform_halt(HALT_ACTION_HALT, ZirconCrashReason::Panic);
  });
}

void PrintTypeDescriptor(const TypeDescriptor& type, const char* prefix = NULL) {
  // TODO(https://fxbug.dev/42056251): Improve logging by interpreting TypeDescriptor values.
  if (prefix) {
    printf("%s:", prefix);
  }
  printf("Type Kind (0x%04hx)\tInfo (0x%04hx)\tName %s\n", type.TypeKind, type.TypeInfo,
         type.TypeName);
}

}  // namespace

extern "C" {

void __ubsan_handle_nonnull_arg(NonNullArgData* Data) {
  auto start = UbsanPanicStart("NULL ARG passed to NonNullarg parameter.", Data->Loc);
}

void __ubsan_handle_type_mismatch_v1(TypeMismatchData* Data, ValueHandle Pointer) {
  auto start = UbsanPanicStart("Type Mismatch", Data->Loc);

  const uintptr_t Alignment = (uintptr_t)1 << Data->LogAlignment;
  const uintptr_t AlignmentMask = Alignment - 1;

  printf("Pointer: 0x%016lx\n", Pointer);
  printf("Alignment: 0x%lx bytes\n", Alignment);

  if (Pointer & AlignmentMask) {
    printf("%s misaligned address 0x%016lx\n", TypeCheckKindMsg(Data->TypeCheckKind), Pointer);
  } else {
    printf("TypeCheck Kind: %s (0x%hhx)\n", TypeCheckKindMsg(Data->TypeCheckKind),
           Data->TypeCheckKind);
  }

  PrintTypeDescriptor(Data->Type);
}

void __ubsan_handle_function_type_mismatch(FunctionTypeMismatchData* Data, ValueHandle Val) {
  auto start = UbsanPanicStart("Function Type Mismatch", Data->Loc);

  PrintTypeDescriptor(Data->Type);
}

#define UBSAN_OVERFLOW_HANDLER(handler_name, op)                            \
  void handler_name(OverflowData* Data, ValueHandle LHS, ValueHandle RHS) { \
    auto start = UbsanPanicStart("Integer " op " overflow\n", Data->Loc);   \
    printf("LHS: 0x%016lx\n", LHS);                                         \
    printf("RHS: 0x%016lx\n", RHS);                                         \
    PrintTypeDescriptor(Data->Type);                                        \
  }

UBSAN_OVERFLOW_HANDLER(__ubsan_handle_add_overflow, "ADD")
UBSAN_OVERFLOW_HANDLER(__ubsan_handle_mul_overflow, "MUL")
UBSAN_OVERFLOW_HANDLER(__ubsan_handle_sub_overflow, "SUB")

void __ubsan_handle_divrem_overflow(OverflowData* Data, ValueHandle LHS, ValueHandle RHS,
                                    ReportOptions Opts) {
  auto start = UbsanPanicStart("Integer DIVREM overflow", Data->Loc);
  printf("LHS: 0x%016lx\n", LHS);
  printf("RHS: 0x%016lx\n", RHS);
  PrintTypeDescriptor(Data->Type);
}

void __ubsan_handle_negate_overflow(OverflowData* Data, ValueHandle OldVal) {
  auto start = UbsanPanicStart("Integer NEGATE overflow", Data->Loc);
  printf("old value: 0x%016lx\n", OldVal);
  PrintTypeDescriptor(Data->Type);
}

void __ubsan_handle_load_invalid_value(InvalidValueData* Data, ValueHandle Val) {
  auto start = UbsanPanicStart("Load invalid value into enum/bool", Data->Loc);
  printf("Val: 0x%016lx\n", Val);
  PrintTypeDescriptor(Data->Type);
}

void __ubsan_handle_implicit_conversion(ImplicitConversionData* Data, ValueHandle Src,
                                        ValueHandle Dst) {
  auto start = UbsanPanicStart("Implicit Conversion", Data->Loc);
  printf("Src: 0x%016lx\n", Src);
  printf("Dst: 0x%016lx\n", Dst);
  PrintTypeDescriptor(Data->FromType, "From");
  PrintTypeDescriptor(Data->ToType, "To");
}

void __ubsan_handle_out_of_bounds(OutOfBoundsData* Data, ValueHandle Index) {
  auto start = UbsanPanicStart("Out of bounds access", Data->Loc);
  printf("Index: 0x%016lx\n", Index);
  PrintTypeDescriptor(Data->ArrayType, "Array");
  PrintTypeDescriptor(Data->IndexType, "Index");
}

void __ubsan_handle_shift_out_of_bounds(ShiftOutOfBoundsData* Data, ValueHandle LHS,
                                        ValueHandle RHS) {
  auto start = UbsanPanicStart("SHIFT overflow", Data->Loc);
  printf("LHS: 0x%016lx\n", LHS);
  printf("RHS: 0x%016lx\n", RHS);
  PrintTypeDescriptor(Data->LHSType, "LHS");
  PrintTypeDescriptor(Data->RHSType, "RHS");
}

void __ubsan_handle_pointer_overflow(PointerOverflowData* Data, ValueHandle Base,
                                     ValueHandle Result) {
  auto start = UbsanPanicStart("POINTER overflow", Data->Loc);
  printf("Base: 0x%016lx\n", Base);
  printf("Result: 0x%016lx\n", Result);
}

void __ubsan_handle_builtin_unreachable(UnreachableData* Data) {
  auto start = UbsanPanicStart("Executed unreachable code", Data->Loc);
}

void __ubsan_handle_alignment_assumption(AlignmentAssumptionData* Data, ValueHandle Pointer,
                                         ValueHandle Alignment, ValueHandle Offset) {
  auto start = UbsanPanicStart("Alignment Assumption violation", Data->Loc);
  PrintTypeDescriptor(Data->Type);
  printf("Pointer: 0x%016lx\n", Pointer);
  printf("Alignment: 0x%016lx\n", Alignment);
  printf("Offset: 0x%016lx\n", Offset);
}

// TODO(https://fxbug.dev/42056251): Add missing handlers:
// * invalid_builtin
// * nonnull_return_v1
// * nullability_return_v1
// * nullability_arg
// * cfi_check_fail
// * cfi_bad_type

// NOTE: The following functions should never be generated in the kernel ubsan:
//  * missing_return
//  * vla_bound_not_positive
//  * float_cast_overflow

}  // extern "C"
}  // namespace ubsan
