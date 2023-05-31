// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

// Undefined Behavior commands.
// These commands can be used to test the undefined behavior sanitizer.
// A kernel compiled with the `kubsan` variant should be able to detect
// each of them.
//
// Most of the functions use inline assembly as an attempt to avoid the compiler
// from optimizing the operations away or throwing warnings.

#include <inttypes.h>
#include <lib/console.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

namespace {

// The compiler cannot assume it knows the return value of Launder.
template <typename T>
T Launder(T x) {
  __asm__("" : "=r"(x) : "0"(x));
  return x;
}

void overflow_signed_int_add() {
  // Signed integer overflow, where the result of a signed integer computation
  // cannot be represented in its type.
  int32_t x = Launder(INT32_MAX);
  int32_t y = Launder(1);

  printf("integer overflow: %d + %d\n", x, y);
  int32_t res = x + y;
  printf("result: %d\n", res);
}

void overflow_signed_int_shift() {
  // Shift operators where the amount shifted is greater or equal to the
  // promoted bit-width of the left hand side or less than zero, or where the
  // left hand side is negative.

  int64_t big_val = Launder(0x1000000);
  size_t shift = Launder(50);

  printf("shift overflowed: %" PRId64 " << %zu\n", big_val, shift);
  int64_t res = big_val << shift;
  printf("result: %" PRId64 "\n", res);
}

void overflow_ptr() {
  // Performing pointer arithmetic which overflows, or where either the old or
  // new pointer value is a null pointer (or in C, when they both are).
  uint8_t local_variable = 0x01;
  uint8_t* ptr = &local_variable;
  size_t overflower = Launder(UINT64_MAX);

  printf("pointer overflow: %p + 0x%zx\n", ptr, overflower);
  uint8_t* newptr = ptr + overflower;
  printf("result: %p\n", newptr);
}

void misaligned_ptr() {
  // Use of a misaligned pointer or creation of a misaligned reference.
  uint64_t aligned = 0;
  uint32_t* addr = reinterpret_cast<uint32_t*>(Launder(reinterpret_cast<uintptr_t>(&aligned)) + 1);

  printf("misaligned pointer access: *%p\n", addr);
  uint32_t val = *addr;
  printf("result: %x\n", val);
}

void array_oob() {
  // Out of bounds array indexing, in cases where the array bound can be
  // statically determined
  uint32_t buf[] = {0, 1, 2};
  size_t index = Launder(3);

  printf("array read out of bounds: buf[%zu]\n", index);
  uint32_t val = buf[index];
  printf("result: %u\n", val);
}

void undefined_bool() {
  // Load of a bool value that is neither true nor false.
  static_assert(sizeof(uint64_t) >= sizeof(bool), "bool is larger than uint64_t");
  uint64_t garbage = Launder(uint64_t{0xdeadbeef});

  printf("loading a bool with value: %" PRIu64 "\n", garbage);

  bool val;
  memcpy(&val, &garbage, sizeof(val));

  bool b = val;
  uint64_t res = 0;
  memcpy(&res, &b, sizeof(b));

  printf("load of bad bool value: %" PRIu64 "\n", res);
}

void unreachable() {
  // Execute unreachable code.
  printf("About to execute unreachable code\n");

  __builtin_unreachable();
}

void undefined_enum() {
  // Load of a value of an enumerated type which is not in the range of
  // representable values for that enumerated type.
  enum Stuff : uint8_t { Foo, Bar, Baz };

  uint32_t garbage = Launder(0xdeadbeef);
  printf("loading an enum with value: %" PRIu32 "\n", garbage);

  Stuff val;
  memcpy(&val, &garbage, sizeof(val));

  Stuff b = val;
  printf("load of invalid enum value: %d\n", b);
}

}  // namespace

static int cmd_ub(int argc, const cmd_args* argv, uint32_t flags);

STATIC_COMMAND_START
STATIC_COMMAND("ub", "trigger undefined behavior", &cmd_ub)
STATIC_COMMAND_END(mem)

static int cmd_usage(const char* cmd_name) {
  printf("usage:\n");
  printf("%s array_oob                : array out of bounds access\n", cmd_name);
  printf("%s misaligned_ptr           : use a misaligned pointer\n", cmd_name);
  printf("%s overflow_ptr             : pointer arithmetic that overflows\n", cmd_name);
  printf("%s overflow_signed_int_add  : signed integer addition that overflows\n", cmd_name);
  printf("%s overflow_signed_int_shift: signed integer shift that overflows\n", cmd_name);
  printf("%s undefined_enum           : use an undefined value in a enum\n", cmd_name);
  printf("%s undefined_bool           : use a bool that is not true nor false\n", cmd_name);
  printf("%s unreachable              : execute unreachable code.\n", cmd_name);

  return ZX_ERR_INTERNAL;
}

static int cmd_ub(int argc, const cmd_args* argv, uint32_t flags) {
  const char* name = argv[0].str;
  if (argc < 2) {
    printf("Not enough arguments.\n");
    return cmd_usage(name);
  }

  if (!strcmp(argv[1].str, "array_oob")) {
    array_oob();
  } else if (!strcmp(argv[1].str, "misaligned_ptr")) {
    misaligned_ptr();
  } else if (!strcmp(argv[1].str, "overflow_ptr")) {
    overflow_ptr();
  } else if (!strcmp(argv[1].str, "overflow_signed_int_add")) {
    overflow_signed_int_add();
  } else if (!strcmp(argv[1].str, "overflow_signed_int_shift")) {
    overflow_signed_int_shift();
  } else if (!strcmp(argv[1].str, "undefined_enum")) {
    undefined_enum();
  } else if (!strcmp(argv[1].str, "undefined_bool")) {
    undefined_bool();
  } else if (!strcmp(argv[1].str, "unreachable")) {
    unreachable();
  } else if (!strcmp(argv[1].str, "help")) {
    return cmd_usage(name);
  }

  return 0;
}
