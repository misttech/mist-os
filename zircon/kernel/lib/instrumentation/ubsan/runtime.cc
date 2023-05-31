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

// LLVM provides no documentation on the ABI between the compiler and
// the runtime.  The set of function signatures here was culled from
// the LLVM sources for the compiler instrumentation and the runtime.
//
// See
// https://github.com/llvm/llvm-project/tree/eb8ebabfb0efcae69e682b592f12366c3b82e78d/compiler-rt/lib/ubsan

namespace {

/// Situations in which we might emit a check for the suitability of a
/// pointer or glvalue. Needs to be kept in sync with CodeGenFunction.h in
/// clang.
enum TypeCheckKind : uint8_t {
  /// Checking the operand of a load. Must be suitably sized and aligned.
  TCK_Load,
  /// Checking the destination of a store. Must be suitably sized and aligned.
  TCK_Store,
  /// Checking the bound value in a reference binding. Must be suitably sized
  /// and aligned, but is not required to refer to an object (until the
  /// reference is used), per core issue 453.
  TCK_ReferenceBinding,
  /// Checking the object expression in a non-static data member access. Must
  /// be an object within its lifetime.
  TCK_MemberAccess,
  /// Checking the 'this' pointer for a call to a non-static member function.
  /// Must be an object within its lifetime.
  TCK_MemberCall,
  /// Checking the 'this' pointer for a constructor call.
  TCK_ConstructorCall,
  /// Checking the operand of a static_cast to a derived pointer type. Must be
  /// null or an object within its lifetime.
  TCK_DowncastPointer,
  /// Checking the operand of a static_cast to a derived reference type. Must
  /// be an object within its lifetime.
  TCK_DowncastReference,
  /// Checking the operand of a cast to a base object. Must be suitably sized
  /// and aligned.
  TCK_Upcast,
  /// Checking the operand of a cast to a virtual base object. Must be an
  /// object within its lifetime.
  TCK_UpcastToVirtualBase,
  /// Checking the value assigned to a _Nonnull pointer. Must not be null.
  TCK_NonnullAssign,
  /// Checking the operand of a dynamic_cast or a typeid expression.  Must be
  /// null or an object within its lifetime.
  TCK_DynamicOperation
};

struct SourceLocation {
  const char* filename;
  uint32_t line;
  uint32_t column;
};

struct TypeDescriptor {
  uint16_t TypeKind;
  uint16_t TypeInfo;
  char TypeName[];
};

struct NonNullArgData {
  SourceLocation Loc;
  SourceLocation AttrLoc;
  int32_t ArgIndex;
};

struct TypeMismatchData {
  SourceLocation Loc;
  const TypeDescriptor& Type;
  uint8_t LogAlignment;
  TypeCheckKind TypeCheckKind;
};

struct UnreachableData {
  SourceLocation Loc;
};

using ValueHandle = uintptr_t;

const char* TypeCheckKindMsg(TypeCheckKind kind) {
  switch (kind) {
    case TCK_Load:
      return "load of";
    case TCK_Store:
      return "store to";
    case TCK_ReferenceBinding:
      return "reference binding to";
    case TCK_MemberAccess:
      return "member access within";
    case TCK_MemberCall:
      return "member call on";
    case TCK_ConstructorCall:
      return "constructor call on";
    case TCK_DowncastPointer:
      return "downcast of";
    case TCK_DowncastReference:
      return "downcast of";
    case TCK_Upcast:
      return "upcast of";
    case TCK_UpcastToVirtualBase:
      return "cast to virtual base of";
    case TCK_NonnullAssign:
      return "_Nonnull binding to";
    case TCK_DynamicOperation:
      return "dynamic operation on";
    default:
      panic("invalid type check kind %d", kind);
  }
}

struct OverflowData {
  SourceLocation Loc;
  const TypeDescriptor& Type;
};

struct ReportOptions {
  // If FromUnrecoverableHandler is specified, UBSan runtime handler is not
  // expected to return.
  bool FromUnrecoverableHandler;
  /// pc/bp are used to unwind the stack trace.
  uintptr_t pc;
  uintptr_t bp;
};

struct InvalidValueData {
  SourceLocation Loc;
  const TypeDescriptor& Type;
};

// Known implicit conversion check kinds.
enum ImplicitConversionCheckKind : uint8_t {
  ICCK_IntegerTruncation = 0,  // Legacy, was only used by clang 7.
  ICCK_UnsignedIntegerTruncation = 1,
  ICCK_SignedIntegerTruncation = 2,
  ICCK_IntegerSignChange = 3,
  ICCK_SignedIntegerTruncationOrSignChange = 4,
};

struct ImplicitConversionData {
  SourceLocation Loc;
  const TypeDescriptor& FromType;
  const TypeDescriptor& ToType;
  ImplicitConversionCheckKind Kind;
};

struct OutOfBoundsData {
  SourceLocation Loc;
  const TypeDescriptor& ArrayType;
  const TypeDescriptor& IndexType;
};

struct ShiftOutOfBoundsData {
  SourceLocation Loc;
  const TypeDescriptor& LHSType;
  const TypeDescriptor& RHSType;
};

struct PointerOverflowData {
  SourceLocation Loc;
};

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
  // TODO(fxbug.dev/105063): Improve logging by interpreting TypeDescriptor values.
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

}  // extern "C"
