// Copyright 2024 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_LIB_INSTRUMENTATION_UBSAN_UBSAN_HANDLERS_H_
#define ZIRCON_KERNEL_LIB_INSTRUMENTATION_UBSAN_UBSAN_HANDLERS_H_

#include <stdint.h>

#include "ubsan-report.h"
#include "ubsan-types.h"

// This implements a minimal ubsan runtime.  This header should be included by
// one and only one translation unit (source file).  The including code must
// also define the interfaces described in "ubsan-report.h".  That file should
// not be included directly but only included via this file and only in the one
// translation unit that defines those interfaces.
//
// The rest of this file is implementation detail.

namespace [[gnu::visibility("hidden")]] ubsan {

// LLVM provides no documentation on the ABI between the compiler and
// the runtime.  The set of function signatures here was culled from
// the LLVM sources for the compiler instrumentation and the runtime.
//
// See
// https://github.com/llvm/llvm-project/tree/eb8ebabfb0efcae69e682b592f12366c3b82e78d/compiler-rt/lib/ubsan

inline void PrintTypeDescriptor(const TypeDescriptor& type, const char* prefix = nullptr) {
  // TODO(https://fxbug.dev/42056251): Improve logging by interpreting TypeDescriptor values.
  Printf("%s%sType Kind (0x%04hx)\tInfo (0x%04hx)\tName %s\n", prefix ? prefix : "",
         prefix ? ":" : "", type.TypeKind, type.TypeInfo, type.TypeName);
}

// All the runtime handlers have unmangled C-style external linkage names.
extern "C" {

// Declare each function `inline` so that it has "vague linkage", i.e. COMDAT
// semantics; that's really just to be entirely proper for a header file in
// case this were included in more than one TU though it ought not be.  Then
// use `[[gnu::noinline]]` to make sure that LTO doesn't try to inline these to
// their call sites, primarily so that the `__builtin_return_address(0)` in the
// default argument to each Report constructor will be a useful call site
// address for each instance.  Since they have vague linkage, the compiler's
// front end usually won't emit them at all in a TU where it doesn't see any
// references.  So also use `[[gnu::used]]` to tell the compiler that each
// function is used by some kind of linkage it can't understand.  When the
// later instrumentation passes add in the calls to the runtime handlers, their
// linkage will resolve happily.  If an individual function is never actually
// called by any generated code, then --gc-sections will drop its COMDAT group.
//
// Vague linkage (via ELF COMDAT) also means that these definitions will
// implicitly have weak linkage, so if there is an alternative ubsan runtime
// implementation linked in (e.g. from the compiler's prebuilt libraries meant
// for the default build environment), its functions will take precedence.
#define UBSAN_HANDLER [[gnu::used, gnu::noinline]] inline void

UBSAN_HANDLER __ubsan_handle_nonnull_arg(NonNullArgData* Data) {
  Report failure("NULL ARG passed to NonNullarg parameter.", Data->Loc);
}

UBSAN_HANDLER __ubsan_handle_type_mismatch_v1(TypeMismatchData* Data, ValueHandle Pointer) {
  Report failure("Type Mismatch", Data->Loc);

  const uintptr_t Alignment = (uintptr_t)1 << Data->LogAlignment;
  const uintptr_t AlignmentMask = Alignment - 1;

  Printf("Pointer: 0x%016lx\n", Pointer);
  Printf("Alignment: 0x%lx bytes\n", Alignment);

  if (Pointer & AlignmentMask) {
    Printf("%s misaligned address 0x%016lx\n", TypeCheckKindMsg(Data->TypeCheckKind), Pointer);
  } else {
    Printf("TypeCheck Kind: %s (0x%hhx)\n", TypeCheckKindMsg(Data->TypeCheckKind),
           Data->TypeCheckKind);
  }

  PrintTypeDescriptor(Data->Type);
}

UBSAN_HANDLER __ubsan_handle_function_type_mismatch(FunctionTypeMismatchData* Data,
                                                    ValueHandle Val) {
  Report failure("Function Type Mismatch", Data->Loc);

  PrintTypeDescriptor(Data->Type);
}

#define UBSAN_OVERFLOW_HANDLER(handler_name, op)                                     \
  UBSAN_HANDLER handler_name(OverflowData* Data, ValueHandle LHS, ValueHandle RHS) { \
    Report failure("Integer " op " overflow\n", Data->Loc);                          \
    Printf("LHS: 0x%016lx\n", LHS);                                                  \
    Printf("RHS: 0x%016lx\n", RHS);                                                  \
    PrintTypeDescriptor(Data->Type);                                                 \
  }

UBSAN_OVERFLOW_HANDLER(__ubsan_handle_add_overflow, "ADD")
UBSAN_OVERFLOW_HANDLER(__ubsan_handle_mul_overflow, "MUL")
UBSAN_OVERFLOW_HANDLER(__ubsan_handle_sub_overflow, "SUB")

#undef UBSAN_OVERFLOW_HANDLER

UBSAN_HANDLER __ubsan_handle_divrem_overflow(OverflowData* Data, ValueHandle LHS, ValueHandle RHS,
                                             ReportOptions Opts) {
  Report failure("Integer DIVREM overflow", Data->Loc);
  Printf("LHS: 0x%016lx\n", LHS);
  Printf("RHS: 0x%016lx\n", RHS);
  PrintTypeDescriptor(Data->Type);
}

UBSAN_HANDLER __ubsan_handle_negate_overflow(OverflowData* Data, ValueHandle OldVal) {
  Report failure("Integer NEGATE overflow", Data->Loc);
  Printf("old value: 0x%016lx\n", OldVal);
  PrintTypeDescriptor(Data->Type);
}

UBSAN_HANDLER __ubsan_handle_load_invalid_value(InvalidValueData* Data, ValueHandle Val) {
  Report failure("Load invalid value into enum/bool", Data->Loc);
  Printf("Val: 0x%016lx\n", Val);
  PrintTypeDescriptor(Data->Type);
}

UBSAN_HANDLER __ubsan_handle_implicit_conversion(ImplicitConversionData* Data, ValueHandle Src,
                                                 ValueHandle Dst) {
  Report failure("Implicit Conversion", Data->Loc);
  Printf("Src: 0x%016lx\n", Src);
  Printf("Dst: 0x%016lx\n", Dst);
  PrintTypeDescriptor(Data->FromType, "From");
  PrintTypeDescriptor(Data->ToType, "To");
}

UBSAN_HANDLER __ubsan_handle_out_of_bounds(OutOfBoundsData* Data, ValueHandle Index) {
  Report failure("Out of bounds access", Data->Loc);
  Printf("Index: 0x%016lx\n", Index);
  PrintTypeDescriptor(Data->ArrayType, "Array");
  PrintTypeDescriptor(Data->IndexType, "Index");
}

UBSAN_HANDLER __ubsan_handle_shift_out_of_bounds(ShiftOutOfBoundsData* Data, ValueHandle LHS,
                                                 ValueHandle RHS) {
  Report failure("SHIFT overflow", Data->Loc);
  Printf("LHS: 0x%016lx\n", LHS);
  Printf("RHS: 0x%016lx\n", RHS);
  PrintTypeDescriptor(Data->LHSType, "LHS");
  PrintTypeDescriptor(Data->RHSType, "RHS");
}

UBSAN_HANDLER __ubsan_handle_pointer_overflow(PointerOverflowData* Data, ValueHandle Base,
                                              ValueHandle Result) {
  Report failure("POINTER overflow", Data->Loc);
  Printf("Base: 0x%016lx\n", Base);
  Printf("Result: 0x%016lx\n", Result);
}

UBSAN_HANDLER __ubsan_handle_builtin_unreachable(UnreachableData* Data) {
  Report failure("Executed unreachable code", Data->Loc);
}

UBSAN_HANDLER __ubsan_handle_alignment_assumption(AlignmentAssumptionData* Data,
                                                  ValueHandle Pointer, ValueHandle Alignment,
                                                  ValueHandle Offset) {
  Report failure("Alignment Assumption violation", Data->Loc);
  PrintTypeDescriptor(Data->Type);
  Printf("Pointer: 0x%016lx\n", Pointer);
  Printf("Alignment: 0x%016lx\n", Alignment);
  Printf("Offset: 0x%016lx\n", Offset);
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

#undef UBSAN_HANDLER

}  // extern "C"
}  // namespace ubsan

#endif  // ZIRCON_KERNEL_LIB_INSTRUMENTATION_UBSAN_UBSAN_HANDLERS_H_
