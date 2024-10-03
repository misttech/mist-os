// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Module for parsing the text output of the symbolizer.

/// A resolved address.
#[derive(Clone, PartialEq)]
pub struct ResolvedAddress {
    /// Address for which source locations were resolved.
    pub addr: u64,
    /// Source locations found at `addr`.
    pub locations: Vec<ResolvedLocation>,
}

impl ResolvedAddress {
    pub(crate) fn parse(s: &str) -> Result<Self, ParseError> {
        let mut addr = None;
        let mut locations = vec![];
        for line in s.lines() {
            let line = line.trim();
            let first_char = line.chars().next();
            if first_char != Some('#') {
                return Err(ParseError::InvalidLineStart { first_char });
            }

            // First strip off the frame number which is meaningless for our manually constructed
            // backtraces.
            let (_frame_num, without_frame_num) = line.split_at(6);

            // The first element of each line is the address which should be the same for all lines.
            let (address_str, without_address) =
                without_frame_num.split_once(" in ").ok_or_else(|| ParseError::FailedSplit {
                    input: without_frame_num.to_string(),
                    infix: " in ",
                })?;
            let address_str = address_str.strip_prefix("0x").ok_or_else(|| {
                ParseError::MissingPrefix { input: address_str.to_string(), prefix: "0x" }
            })?;
            let parsed_address = u64::from_str_radix(address_str, 16)
                .map_err(|error| ParseError::HexDecode { input: address_str.to_string(), error })?;
            addr = Some(parsed_address);

            // The remainder of the line is specific to each source location.
            locations.push(ResolvedLocation::parse(without_address)?);
        }
        if locations.is_empty() {
            return Err(ParseError::NoLinesFound);
        }
        let addr = addr.expect("addr will be parsed if at least one line parsed successfully");
        Ok(Self { addr, locations })
    }
}

impl std::fmt::Debug for ResolvedAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedAddress")
            .field("addr", &format_args!("0x{:x}", self.addr))
            .field("lines", &self.locations)
            .finish()
    }
}

/// A single source location resolved from an address.
#[derive(Clone, PartialEq)]
pub struct ResolvedLocation {
    /// The function name of the location.
    pub function: String,
    /// The source file for the referenced function.
    pub file: String,
    /// The source line number on which this address is found.
    pub line: u32,
    /// The name of the library (if any) in which this location is found.
    pub library: Option<String>,
    /// The offset within the library's file where this location is found.
    pub library_offset: u64,
}

impl ResolvedLocation {
    fn parse(s: &str) -> Result<Self, ParseError> {
        // The rest of the line has the format FUNCTION FILE:LINE <LIBRARY>+OFFSET OPTIONAL_SUFFIX.
        // The function name has arbitrary formatting, so split things off from the end of the line.
        let (function_and_source_loc, library_and_suffix) = s
            .rsplit_once(" <")
            .ok_or_else(|| ParseError::FailedSplit { input: s.to_string(), infix: " <" })?;
        let (library_name, library_offset_and_message) =
            library_and_suffix.split_once(">+").ok_or_else(|| ParseError::FailedSplit {
                input: library_and_suffix.to_string(),
                infix: ">+",
            })?;
        let library = if library_name.is_empty() { None } else { Some(library_name.to_string()) };
        let (raw_offset, _suffix) =
            library_offset_and_message.split_once(" ").unwrap_or((library_offset_and_message, ""));
        let raw_offset = raw_offset.strip_prefix("0x").ok_or_else(|| {
            ParseError::MissingPrefix { input: raw_offset.to_string(), prefix: "0x" }
        })?;
        let library_offset = u64::from_str_radix(raw_offset, 16)
            .map_err(|error| ParseError::HexDecode { input: raw_offset.to_string(), error })?;

        let (function_and_file, raw_line_no) =
            function_and_source_loc.rsplit_once(":").ok_or_else(|| ParseError::FailedSplit {
                input: function_and_source_loc.to_string(),
                infix: ":",
            })?;
        let raw_line_no = raw_line_no.trim();
        let line = raw_line_no
            .parse()
            .map_err(|error| ParseError::DecimalDecode { input: raw_line_no.to_string(), error })?;

        // NOTE: this won't properly handle spaces in paths. Hopefully we don't encounter any.
        let (untrimmed_function, untrimmed_file) =
            function_and_file.rsplit_once(" ").ok_or_else(|| ParseError::FailedSplit {
                input: function_and_file.to_string(),
                infix: " ",
            })?;

        Ok(Self {
            function: untrimmed_function.trim().to_string(),
            file: untrimmed_file.trim().to_string(),
            line,
            library,
            library_offset,
        })
    }
}

impl std::fmt::Debug for ResolvedLocation {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolvedLocation")
            .field("function", &self.function)
            .field("file", &self.file)
            .field("line", &self.line)
            .field("library", &self.library)
            .field("library_offset", &format_args!("0x{:x}", self.library_offset))
            .finish()
    }
}

/// Errors that can occur when running the symbolizer.
#[derive(Debug, thiserror::Error)]
pub enum ParseError {
    /// A line started with something other than expected symbolizer output.
    #[error("Line started with '{first_char:?}' instead of '#'.")]
    InvalidLineStart {
        /// First character of the line encountered.
        first_char: Option<char>,
    },

    /// Failed to split a string in two.
    #[error("Couldn't split `{input}` on `{infix}`.")]
    FailedSplit {
        /// Value without expected infix.
        input: String,
        /// Expected infix.
        infix: &'static str,
    },

    /// Failed to strip a prefix.
    #[error("Couldn't remove `{prefix}` from beginning of `{input}`.")]
    MissingPrefix {
        /// Value without expected prefix.
        input: String,
        /// Expected prefix.
        prefix: &'static str,
    },

    /// Failed to parse a hex string as an integer.
    #[error("Couldn't parse `{input}` as a hexadecimal integer.")]
    HexDecode {
        /// Value which was used as input.
        input: String,
        /// Error returned by parser.
        #[source]
        error: std::num::ParseIntError,
    },

    /// Failed to parse a decimal string as an integer.
    #[error("Couldn't parse `{input}` as a decimal integer.")]
    DecimalDecode {
        /// Value which was used as input.
        input: String,
        /// Error returned by parser.
        #[source]
        error: std::num::ParseIntError,
    },

    /// Empty input.
    #[error("No lines were encountered with valid symbolizer output.")]
    NoLinesFound,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_simple_single_line() {
        let line = "#0    0x000001bce919b461 in print_fn_ptr_bin::to_be_symbolized_one() ../../src/developer/ffx/lib/symbolize/tests/print_fn_ptr.rs:10 <>+0xc461";
        assert_eq!(
            ResolvedAddress::parse(line).unwrap(),
            ResolvedAddress {
                addr: 0x000001bce919b461,
                locations: vec![ResolvedLocation {
                    function: "print_fn_ptr_bin::to_be_symbolized_one()".to_string(),
                    file: "../../src/developer/ffx/lib/symbolize/tests/print_fn_ptr.rs".to_string(),
                    line: 10,
                    library: None,
                    library_offset: 0xc461,
                }],
            }
        );
    }

    #[test]
    fn parse_multi_line_with_suffix_messages() {
        // Adapted from //tools/symbolizer/test_cases/multithread.out
        let multi_line = r#"#4.2  0x0000035ff4da7460 in std::__2::__invoke<void(*)(const zx::event*, const zx::event*, int, int), zx::event*, zx::event*, int, int>(void (*)(const zx::event*, const zx::event*, int, int)&&, zx::event*&&, zx::event*&&, int&&, int&&) ../../prebuilt/third_party/clang/mac-x64/include/c++/v1/type_traits:3899 <<VMO#41864=backtrace_request>>+0x1460 sp 0x19dc512afc0
#4.1  0x0000035ff4da7460 in std::__2::__thread_execute<std::__2::unique_ptr<std::__2::__thread_struct, std::__2::default_delete<std::__2::__thread_struct>>, void(*)(const zx::event*, const zx::event*, int, int), zx::event*, zx::event*, int, int, 2, 3, 4, 5>(std::__2::tuple<std::__2::unique_ptr<std::__2::__thread_struct, std::__2::default_delete<std::__2::__thread_struct> >, void (*)(const zx::event *, const zx::event *, int, int), zx::event *, zx::event *, int, int>&, std::__2::__tuple_indices<2, 3, 4, 5>) ../../prebuilt/third_party/clang/mac-x64/include/c++/v1/thread:280 <<VMO#41864=backtrace_request>>+0x1460 sp 0x19dc512afc0
#4    0x0000035ff4da7460 in std::__2::__thread_proxy<std::__2::tuple<std::__2::unique_ptr<std::__2::__thread_struct, std::__2::default_delete<std::__2::__thread_struct>>, void(*)(const zx::event*, const zx::event*, int, int), zx::event*, zx::event*, int, int>>(void*) ../../prebuilt/third_party/clang/mac-x64/include/c++/v1/thread:291 <<VMO#41864=backtrace_request>>+0x1460 sp 0x19dc512afc0"#;
        assert_eq!(
            ResolvedAddress::parse(multi_line).unwrap(),
            ResolvedAddress {
                addr: 0x0000035ff4da7460,
                locations: vec![
                    ResolvedLocation {
                        function: "std::__2::__invoke<void(*)(const zx::event*, const zx::event*, int, int), zx::event*, zx::event*, int, int>(void (*)(const zx::event*, const zx::event*, int, int)&&, zx::event*&&, zx::event*&&, int&&, int&&)".to_string(),
                        file: "../../prebuilt/third_party/clang/mac-x64/include/c++/v1/type_traits".to_string(),
                        line: 3899,
                        library: Some("<VMO#41864=backtrace_request>".to_string()),
                        library_offset: 0x1460,
                    },
                    ResolvedLocation {
                        function: "std::__2::__thread_execute<std::__2::unique_ptr<std::__2::__thread_struct, std::__2::default_delete<std::__2::__thread_struct>>, void(*)(const zx::event*, const zx::event*, int, int), zx::event*, zx::event*, int, int, 2, 3, 4, 5>(std::__2::tuple<std::__2::unique_ptr<std::__2::__thread_struct, std::__2::default_delete<std::__2::__thread_struct> >, void (*)(const zx::event *, const zx::event *, int, int), zx::event *, zx::event *, int, int>&, std::__2::__tuple_indices<2, 3, 4, 5>)".to_string(),
                        file: "../../prebuilt/third_party/clang/mac-x64/include/c++/v1/thread".to_string(),
                        line: 280,
                        library: Some("<VMO#41864=backtrace_request>".to_string()),
                        library_offset: 0x1460,
                    },
                    ResolvedLocation {
                        function: "std::__2::__thread_proxy<std::__2::tuple<std::__2::unique_ptr<std::__2::__thread_struct, std::__2::default_delete<std::__2::__thread_struct>>, void(*)(const zx::event*, const zx::event*, int, int), zx::event*, zx::event*, int, int>>(void*)".to_string(),
                        file: "../../prebuilt/third_party/clang/mac-x64/include/c++/v1/thread".to_string(),
                        line: 291,
                        library: Some("<VMO#41864=backtrace_request>".to_string()),
                        library_offset: 0x1460,
                    },
                ]
            }
        )
    }
}
