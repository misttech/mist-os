# Adding a new object type

## Syscall Interface

The syscall interface is defined in `.fidl` files in [`//zircon/vdso/`](/zircon/vdso/). To add
a new object type to that FIDL definition:

 - add a new member to the `ObjType` enum in
   [`//zircon/vdso/zx_common.fidl`](/zircon/vdso/zx_common.fidl)
 - create a new FIDL file to defined object-specific syscalls in
   [`//zircon/vdso/`](/zircon/vdso/), following the pattern in other files like
   [`channel.fidl`](/zircon/vdso/channel.fidl).

The FIDL toolchain bakes in knowledge of object types. This isn't elegant, but
it avoids some circular dependencies. You'll have to update parts of the FIDL
toolchain to understand your new object type. In particular, update:

 - `HandleSubtype` in
   [`//tools/fidl/fidlc/src/properties.h`](/tools/fidl/fidlc/src/properties.h)
 - `NameHandleSubtype` in [`//tools/fidl/fidlc/src/names.cc`](/tools/fidl/fidlc/src/names.cc)
 - `HandleType` in
   [`//tools/fidl/abi-compat/src/compare/handle.rs`](/tools/fidl/abi-compat/src/compare/handle.rs)
 - `handle-subtype` in [`//tools/fidl/fidlc/schema.json`](/tools/fidl/fidlc/schema.json)
 - `GoodHandleSubtype` in
   [`//tools/fidl/fidlc/tests/types_tests.cc`](/tools/fidl/fidlc/tests/types_tests.cc)
 - `HandleSubtype` and `ObjectType` consts in
   [`//tools/fidl/lib/fidlgen/types.go`](/tools/fidl/lib/fidlgen/types.go)
 - `handleSubtypes`, `handleSubtypesFdomain` and `objectTypeConsts` in
   [`//tools/fidl/fidlgen_rust/codegen/ir.go`](/tools/fidl/fidlgen_rust/codegen/ir.go)

## Kernel Implementation

*TODO(b/383761360):* add it in the kernel

## Userspace Support

Userspace developers want more than the raw C ABI, they expect language bindings and tooling support.

### C++

The C++ syscall bindings are implemented in
[`//zircon/system/ulib/zx/`](/zircon/system/ulib/zx/). Implement an appropriate `zx::object`
subclass for your new object type.

### Rust

The Rust syscall bindings are implemented in [`//sdk/rust/zx/`](/sdk/rust/zx/). Implement an
appropriate `Handle` wrapper type for your new object type.

### fidlcat

The `ffx debug fidl` aka `fidlcat` tool is roughly analogous to `strace` on
Linux. It's based on the `fidl_codec` library implemented in
[`//src/lib/fidl_codec/`](/src/lib/fidl_codec/). Teaching it about a new object type includes updating:

 - `ShortObjTypeName` in [`display_handle.cc`](/src/lib/fidl_codec/display_handle.cc)
 - `PrettyPrinter::DisplayObjType` in [`printer.cc`](/src/lib/fidl_codec/printer.cc)
