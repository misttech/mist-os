// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub use ffx_validation_proc_macro::schema;

#[cfg(test)]
mod test {
    use super::*;
    use crate::schema::Schema;

    // Marker structs
    struct Optional;
    struct RustType;
    struct InlineStruct;
    struct Tuple;

    struct Union;

    struct Generic<T>(T);
    struct NotASchema<T>(T);

    // Schema macro syntax tests.
    schema! {
        type RustType = Option<u32>;
        type Optional = String?;
        type InlineStruct = struct {
            a: u32,
            b: u32,
            c: u32,
        };
        type Tuple = (RustType, Optional, InlineStruct);

        type Union = RustType | Optional | InlineStruct;

        type Generic<T> where T: Schema + 'static = T;
        impl<T> for Generic<NotASchema<T>> where T: Schema + 'static = T;
    }
}
