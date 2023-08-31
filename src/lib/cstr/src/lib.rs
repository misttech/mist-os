// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! This crate contains the [`cstr`] macro for making `&'static CStr` types out of string literals.

#![deny(missing_docs)]

// Re-export libc to be used from the c_char macro
#[doc(hidden)]
pub mod __reexport {
    pub use static_assertions::*;
}

/// Creates a `&'static CStr` from a string literal.
#[macro_export]
macro_rules! cstr {
    ($s:expr) => {
        // `concat` macro always produces a static string literal.
        // It is always safe to create a CStr from a null-terminated string.
        // If there are interior null bytes, the string will just end early.
        {
            const CSTR: Result<&'static ::core::ffi::CStr, ::core::ffi::FromBytesUntilNulError> =
                ::core::ffi::CStr::from_bytes_until_nul(concat!($s, "\0").as_bytes());
            $crate::__reexport::const_assert!(CSTR.is_ok());
            // SAFETY: from_bytes_until_nul will only produce an error if there is no nul byte.
            // Since we add one, unwrap should always be ok. We also assert at compile time just in
            // case this assumption changes from under us.
            unsafe { CSTR.unwrap_unchecked() }
        }
    };
}

#[cfg(test)]
mod tests {
    use {super::cstr, std::ffi};

    #[test]
    fn cstr() {
        let cstring = ffi::CString::new("test string").expect("CString::new failed");
        let cstring_cstr = cstring.as_c_str();
        let cstr = cstr!("test string");

        assert_eq!(cstring_cstr, cstr);
    }

    #[test]
    fn cstr_not_equal() {
        let cstring = ffi::CString::new("test string").expect("CString::new failed");
        let cstring_cstr = cstring.as_c_str();
        let cstr = cstr!("different test string");

        assert_ne!(cstring_cstr, cstr);
    }

    #[test]
    fn cstr_early_null() {
        let cstring = ffi::CString::new("test").expect("CString::new failed");
        let cstring_cstr = cstring.as_c_str();
        let cstr = cstr!("test\0 string");

        assert_eq!(cstring_cstr, cstr);
    }
}
