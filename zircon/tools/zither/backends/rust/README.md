# The Zither Rust Backend

The Zither Rust backend is gives Rust data layout bindings.

## Output layout

Given a FIDL library by the name of `id1.id2.....idn`, one crate with name
`fidl-data-id1-id2-...-idn` is generated: it contains a `${filename}.rs` for
each original FIDL source file and a crate root `lib.rs`.

## GN integration

`${fidl_target}_zither.rust` gives a `rustc_library()` defining the generated
crate.

## Bindings

Any declaration type not mentioned below is ignored.

### Built-in types

| FIDL type     | Rust type   |
| ------------- | ----------- |
| `int8`        | `i8`        |
| `int16`       | `i16`       |
| `int32`       | `i32`       |
| `int64`       | `i64`       |
| `uint8`       | `u8`        |
| `uint16`      | `u16`       |
| `uint32`      | `u32`       |
| `uint64`      | `u64`       |
| `bool`        | `bool`      |
| `string`      | `&str`      |
| `uchar`       | `u8`        |
| `usize64`     | `usize`     |
| `uintptr64`   | `usize`     |
| `array<T, N>` | `[T'; N]`   |

Note that FIDL `string`s are only permitted as constants.

### Constants

```fidl
const INT_CONST uint32 = 10;  // Or any integral type.
const STR_CONST string = "string constant";
```

yields

```rust
pub const INT_CONST: uint32  = 10;
pub const STR_CONST: &str  = "string constant";
```

### Enums

```fidl
type MyEnum = enum : int8 {  // Or any valid integral type
    ZERO = 0;
    ONE = 1;
};
```

yields

```rust
#[repr(i8)]
pub enum MyEnum {
    Zero = 0,
    One = 1,
}
```

### Bits

```fidl
type MyBits = bits : uint8 {  // Or any valid integral type
    ONE = 1;
    TWO = 2;
    FOUR = 4;
};
```

yields

```
bitflags! {
    pub struct MyBits : u8 {
    const ONE = 1 << 0;
    const TWO = 1 << 1;
    const FOUR = 1 << 2;
  }
}
```

### Structs

```fidl
type MyStruct = struct {
    member_a uint64;
    member_b bool;
};
```

yields

```rust
#[repr(C)]
pub struct MyStruct {
    pub member_a: uint64,
    pub member_b: bool,
}
```

### Aliases

```fidl
alias MyAlias = MyType;
```

yields

```rust
pub type MyAlias = MyType;
```

### Overlays

```fidl
type MyOverlay = strict overlay {
    1: a MyOverlayStructVariant;
    2: b uint32;
};
```

yields

```rust
#[repr(C)]
#[derive(AsBytes, Clone, Copy)]
pub struct MyOverlay {
    pub discriminant: MyOverlayDiscriminant,
    pub variant: MyOverlayVariant,
}

#[repr(u64)]
#[derive(AsBytes, Clone, Copy, Debug, Eq, PartialEq)]
pub enum MyOverlayDiscriminant {
    A = 1,
    B = 2,
}

// TODO(https://github.com/rust-lang/rust/issues/49804): Define anonymously.
#[repr(C)]
#[derive(AsBytes, Clone, Copy)]
pub union MyOverlayVariant {
    pub a: MyOverlayStructVariant,
    pub b: u32,
}

impl MyOverlay {
    pub fn is_a(&self) -> bool {
        self.discriminant == MyOverlayDiscriminant::A
    }

    pub fn as_a(&mut self) -> Option<&MyOverlayStructVariant> {
        if self.is_a() {
            return None;
        }
        unsafe { Some(&mut self.variant.a) }
    }

    pub fn is_b(&self) -> bool {
        self.discriminant == MyOverlayDiscriminant::B
    }

    pub fn as_b(&mut self) -> Option<&u32> {
        if self.is_b() {
            return None;
        }
        unsafe { Some(&mut self.variant.b) }
    }
}
```
