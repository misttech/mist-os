// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// DO NOT EDIT. Generated from FIDL library
//   zither.experimental.zx.types (//zircon/tools/zither/testdata/experimental_zx_types/experimental_zx_types.test.fidl)
// by zither, a Fuchsia platform tool.

package types

import (
	"unsafe"
)

// 'a'
const CharConst byte = 97

const SizeConst uint = 100

const UintptrConst uintptr = 0x1234abcd5678ffff

type StructWithPrimitives struct {
	CharField    byte
	SizeField    uint
	UintptrField uintptr
}

type Uint8Alias = uint8

type StructWithPointers struct {
	U64ptr   *uint64
	Charptr  *byte
	Usizeptr *uint
	Byteptr  *uint8
	Voidptr  unsafe.Pointer
	Aliasptr *Uint8Alias
}

type StructWithStringArrays struct {
	Str  [10]byte
	Strs [4][6]byte
}
