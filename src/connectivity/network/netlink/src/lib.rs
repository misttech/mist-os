// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// Temporary functions for initial setup.
fn add(a: i16, b: i16) -> i16 {
    return a + b;
}

fn subtract(a: i16, b: i16) -> i16 {
    return a - b;
}

#[cfg(test)]
mod tests {
    use crate::{add, subtract};

    #[fuchsia::test]
    fn test_add() {
        let a: i16 = 10;
        let b: i16 = 5;

        assert_eq!(add(a, b), 15);
    }

    #[fuchsia::test]
    fn test_subtract() {
        let a: i16 = 15;
        let b: i16 = 8;

        assert_eq!(subtract(a, b), 7);
    }
}
