// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// DO NOT EDIT. Generated from FIDL library
//   zither.experimental.zx.types
//   (//zircon/tools/zither/testdata/experimental_zx_types/experimental_zx_types.test.fidl)
// by zither, a Fuchsia platform tool.

#ifndef LIB_ZITHER_EXPERIMENTAL_ZX_TYPES_C_EXPERIMENTAL_ZX_TYPES_H_
#define LIB_ZITHER_EXPERIMENTAL_ZX_TYPES_C_EXPERIMENTAL_ZX_TYPES_H_

#include <stddef.h>
#include <stdint.h>

#if defined(__cplusplus)
extern "C" {
#endif

// 'a'
#define ZITHER_EXPERIMENTAL_ZX_TYPES_CHAR_CONST ((char)(97u))

#define ZITHER_EXPERIMENTAL_ZX_TYPES_SIZE_CONST ((size_t)(100u))

#define ZITHER_EXPERIMENTAL_ZX_TYPES_UINTPTR_CONST ((uintptr_t)(0x1234abcd5678ffffu))

typedef struct {
  char char_field;
  size_t size_field;
  uintptr_t uintptr_field;
} zither_experimental_zx_types_struct_with_primitives_t;

typedef uint8_t zither_experimental_zx_types_uint8_alias_t;

typedef struct {
  uint64_t* u64ptr;
  char* charptr;
  size_t* usizeptr;
  uint8_t* byteptr;
  void* voidptr;
  zither_experimental_zx_types_uint8_alias_t* aliasptr;
} zither_experimental_zx_types_struct_with_pointers_t;

typedef struct {
  char str[10];
  char strs[6][4];
} zither_experimental_zx_types_struct_with_string_arrays_t;

#if defined(__cplusplus)
}
#endif

#endif  // LIB_ZITHER_EXPERIMENTAL_ZX_TYPES_C_EXPERIMENTAL_ZX_TYPES_H_
