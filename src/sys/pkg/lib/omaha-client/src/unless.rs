// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! The Unless trait for a more fluent use of Option::unwrap_or().
//!
//! Specificially, this is intended to be used in cases where the "default" value is almost always
//! the value in use, and the Option is rarely set.
//!
//! ```
//! use omaha_client::unless::Unless;
//! // This implies that |some_option| is usually set, and "default" is there in case it's not.
//! let some_option_usually_set = Some("string");
//! let value = some_option_usually_set.unwrap_or("default");
//! assert_eq!("string", value);
//!
//! // Whereas this implies that "default" is the common case, and |some_option| is an override.
//! let some_option_usually_unset = None;
//! let value = "default".unless(some_option_usually_unset);
//! assert_eq!("default", value);
//! ```

pub trait Unless: Sized {
    // unless returns the value of self, unless the option is Some value.
    //
    // # Example
    //
    // ```
    // assert_eq!("default", "default".unless(None));
    // assert_eq!("other", "default".unless(Some("other")));
    // ```
    fn unless(self, option: Option<Self>) -> Self;
}

/// Provide a blanket implementation for all Sized types.
impl<T: Sized> Unless for T {
    fn unless(self, option: Option<Self>) -> Self {
        option.unwrap_or(self)
    }
}

#[cfg(test)]
mod tests {
    use super::Unless;

    #[test]
    fn tests() {
        assert_eq!("default", "default".unless(None));
        assert_eq!("other", "default".unless(Some("other")));
    }
}
