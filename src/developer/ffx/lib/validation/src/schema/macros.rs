// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#[cfg(test)]
mod test {
    use crate::schema::Schema;
    use ffx_validation_proc_macro::schema;

    // Marker structs
    struct Optional;
    struct RustType;
    struct InlineStruct;
    struct Tuple;

    struct Union;

    struct Generic<T>(T);
    struct NotASchema<T>(T);

    struct Transparent;

    #[allow(dead_code)]
    struct ForeignType;

    struct Enum;

    // Schema macro syntax tests.
    schema! {
        type RustType = Option<u32>;
        type Optional = String?;
        type InlineStruct = struct {
            a: u32,
            b: u32,
            c: u32,
            d?: u64,
        };
        type Tuple = (RustType, Optional, InlineStruct);

        type Union = RustType | Optional | InlineStruct;

        type Generic<T> where T: Schema + 'static = T;
        impl<T> for Generic<NotASchema<T>> where T: Schema + 'static = T;

        #[transparent] type Transparent = u32;

        fn my_lone_function = Option<InlineStruct>;
        #[foreign(ForeignType)] fn foreign_schema = (i32, i32, i32);

        type Enum = enum {
            A, B, C(String), D { field: u32, b: u32 },
        };
    }
}
