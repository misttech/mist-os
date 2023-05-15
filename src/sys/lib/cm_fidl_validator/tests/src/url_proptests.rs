// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use {
    cm_fidl_validator::check_url, cm_types::MAX_URL_LENGTH, lazy_static::lazy_static,
    proptest::prelude::*, url::Url,
};

#[macro_use]
mod url_regex {
    macro_rules! or {
        () => {
            "|"
        };
    }
    macro_rules! zero_or_more {
        ($s:expr) => {
            concat!($s, "*")
        };
    }
    macro_rules! one_or_more {
        ($s:expr) => {
            concat!($s, "+")
        };
    }
    macro_rules! repeat {
        ($count:literal, $s:expr) => {
            concat!($s, "{", stringify!($count), "}")
        };
    }
    macro_rules! range {
        ($min:literal, $max:literal, $s:expr) => {
            concat!($s, "{", stringify!($min), ",", stringify!($max), "}")
        };
    }
    macro_rules! optional {
        ($s:expr) => {
            concat!($s, "?")
        };
    }
    macro_rules! group { ($($e:expr),* $(,)?) => { concat!("(", $($e),*, ")") }; }
    macro_rules! any { ($f:expr, $($e:expr),* $(,)?) => { group!($f, $(or!(), $e),*) }; }
    macro_rules! zero_or_more_group { ($($e:expr),* $(,)?) => { zero_or_more!(group!($($e),*,)) }; }
    macro_rules! repeat_group { ($count:literal, $($e:expr),* $(,)?) => { repeat!($count, group!($($e),*,)) }; }
    macro_rules! range_group { ($min:literal, $max:literal, $($e:expr),* $(,)?) => { range!($min, $max, group!($($e),*,)) }; }
    macro_rules! optional_group { ($($e:expr),* $(,)?) => { optional!(group!($($e),*,)) }; }

    macro_rules! lit {
        ($id:ident, $s:expr) => {
            macro_rules! $id {
                () => {
                    $s
                };
            }
        };
    }

    // RFC 3986: Uniform Resource Identifier (URI): Generic Syntax
    //
    // Appendix A.  Collected ABNF for URI
    // https://www.rfc-editor.org/rfc/rfc3986#appendix-A
    //
    //   path          = path-abempty    ; begins with "/" or is empty
    //   / path-absolute   ; begins with "/" but not "//"
    //   / path-noscheme   ; begins with a non-colon segment
    //   / path-rootless   ; begins with a segment
    //   / path-empty      ; zero characters
    //
    //   gen-delims    = ":" / "/" / "?" / "#" / "[" / "]" / "@"

    lit!(DIGIT, "[0-9]");
    lit!(ALPHA, "[a-zA-Z]");
    lit!(HEXDIG, "[0-9a-fA-F]");
    lit!(ALPHA_NO_X, "[a-wyzA-WYZ]");
    lit!(DOT, "[.]");

    // unreserved    = ALPHA / DIGIT / "-" / "." / "_" / "~"
    lit!(unreserved, any!(ALPHA!(), DIGIT!(), "[-._~]"));
    lit!(unreserved_no_dot, any!(ALPHA!(), DIGIT!(), "[-_~]"));
    lit!(unreserved_no_x_or_digit_dot, any!(ALPHA_NO_X!(), "[-_~]"));

    // pct-encoded   = "%" HEXDIG HEXDIG
    //
    // Note that:
    //   lit!(pct_encoded, group!("%", HEXDIG!(), HEXDIG!()));
    // would generate encoded characters that can be invalid in certain
    // contexts.
    //
    // The following regex limits proptest generated values to a few ALPHA
    // equivalents: %80-%89 is "P"-"Y".
    lit!(pct_encoded, group!("%8[0-9]"));

    // sub-delims    = "!" / "$" / "&" / "'" / "(" / ")"
    // / "*" / "+" / "," / ";" / "="
    lit!(sub_delims, "[!$&'()*+,;=]");

    // userinfo      = *( unreserved / pct-encoded / sub-delims / ":" )
    lit!(userinfo, zero_or_more!(any!(unreserved!(), pct_encoded!(), sub_delims!(), ":")));

    // dec-octet     = DIGIT                 ; 0-9
    //   / %x31-39 DIGIT         ; 10-99
    //   / "1" 2DIGIT            ; 100-199
    //   / "2" %x30-34 DIGIT     ; 200-249
    //   / "25" %x30-35          ; 250-255
    lit!(
        dec_octet,
        any!(
            DIGIT!(),
            group!("[1-9]", DIGIT!()),
            group!("1", repeat!(2, DIGIT!())),
            group!("2[0-4]", DIGIT!()),
            group!("25[0-5]")
        )
    );

    // IPv4address   = dec-octet "." dec-octet "." dec-octet "." dec-octet
    lit!(
        IPv4address,
        group!(dec_octet!(), DOT!(), dec_octet!(), DOT!(), dec_octet!(), DOT!(), dec_octet!())
    );

    // h16           = 1*4HEXDIG
    lit!(h16, group!(range!(1, 4, HEXDIG!())));

    // ls32          = ( h16 ":" h16 ) / IPv4address
    lit!(ls32, any!(group!(h16!(), ":", h16!()), IPv4address!()));

    // IPv6address   =                            6( h16 ":" ) ls32
    //               /                       "::" 5( h16 ":" ) ls32
    //               / [               h16 ] "::" 4( h16 ":" ) ls32
    //               / [ *1( h16 ":" ) h16 ] "::" 3( h16 ":" ) ls32
    //               / [ *2( h16 ":" ) h16 ] "::" 2( h16 ":" ) ls32
    //               / [ *3( h16 ":" ) h16 ] "::"    h16 ":"   ls32
    //               / [ *4( h16 ":" ) h16 ] "::"              ls32
    //               / [ *5( h16 ":" ) h16 ] "::"              h16
    //               / [ *6( h16 ":" ) h16 ] "::"
    lit!(
        IPv6address,
        any!(
            group!(repeat_group!(6, h16!(), ":"), ls32!()),
            group!("::", repeat_group!(5, h16!(), ":"), ls32!()),
            group!(optional!(h16!()), "::", repeat_group!(3, h16!(), ":"), ls32!()),
            group!(
                optional_group!(range_group!(0, 1, h16!(), ":"), h16!()),
                "::",
                repeat_group!(3, h16!(), ":"),
                ls32!()
            ),
            group!(
                optional_group!(range_group!(0, 2, h16!(), ":"), h16!()),
                "::",
                repeat_group!(2, h16!(), ":"),
                ls32!()
            ),
            group!(
                optional_group!(range_group!(0, 3, h16!(), ":"), h16!()),
                "::",
                h16!(),
                ":",
                ls32!()
            ),
            group!(optional_group!(range_group!(0, 4, h16!(), ":"), h16!()), "::", ls32!()),
            group!(optional_group!(range_group!(0, 5, h16!(), ":"), h16!()), "::", h16!()),
            group!(optional_group!(range_group!(0, 6, h16!(), ":"), h16!()), "::"),
        )
    );

    // IPvFuture     = "v" 1*HEXDIG "." 1*( unreserved / sub-delims / ":" )
    //
    // For proptest, arbitrarily limit range max to 10.
    // lit!(IPvFuture, group!("v", range!(1, 10, HEXDIG!()), DOT!(), range!(1, 10, any!(unreserved!(), sub_delims!(), ":"))));
    //
    // Url::parse() does not support IPvFuture.

    // IP-literal    = "[" ( IPv6address / IPvFuture  ) "]"
    // Url::parse() does not support IPvFuture.
    lit!(IP_literal, group!("\\[", IPv6address!(), "\\]"));

    // reg-name      = *( unreserved / pct-encoded / sub-delims )
    //
    // Note that:
    //   lit!(reg_name, zero_or_more!(any!(unreserved!(), pct_encoded!(), sub_delims!())));
    // may generate invalid IDNA values (max length 253, and 1-63 characters
    // between dots.
    //
    // The following regex limits proptest generated values to valid IDNA
    // lengths and valid domains. It tests both long names between dots, and
    // domains with many dots.
    //
    // To avoid url::Url::parse() `InvalidIpv4Address` errors, it should not
    // start with a digit. (I _think_ the URL spec explicitly allows domain
    // names that start with digits, but url::Url::parse() seems to try to parse
    // them as IPv4 addresses, and then fails when they don't fit that pattern.
    // This may be a bug in url::Url::parse?)
    //
    // To avoid url::Url::parse() `IdnaError` errors from `reg-name`
    // dot-separated items that begin with (case-insensitive) `xn--`, the
    // regular expression also eliminates `x` and `X` as a prefix.
    lit!(
        reg_name,
        any!(
            group!(
                group!(unreserved_no_x_or_digit_dot!(), range!(0, 62, unreserved_no_dot!())),
                range_group!(
                    0,
                    2,
                    DOT!(),
                    unreserved_no_x_or_digit_dot!(),
                    range!(0, 62, unreserved_no_dot!())
                ),
            ),
            group!(
                group!(unreserved_no_x_or_digit_dot!(), range!(0, 8, unreserved_no_dot!())),
                range_group!(
                    0,
                    30,
                    DOT!(),
                    unreserved_no_x_or_digit_dot!(),
                    range!(0, 8, unreserved_no_dot!())
                ),
            ),
        )
    );

    // host          = IP-literal / IPv4address / reg-name
    lit!(host, any!(IP_literal!(), IPv4address!(), reg_name!(),));

    // port          = *DIGIT
    //
    // Note that:
    //   lit!(port, zero_or_more!(DIGIT!()));
    // would generate invalid port numbers.
    //
    // The following regex limits proptest generated values to valid port numbers:
    lit!(port, "([1-9]|[1-9][0-9]|[1-9][0-9][0-9]|[1-9][0-9][0-9][0-9])");

    // authority     = [ userinfo "@" ] host [ ":" port ]
    lit!(
        authority,
        group!(optional!(group!(userinfo!(), "@")), host!(), optional!(group!(":", port!())),)
    );

    // pchar         = unreserved / pct-encoded / sub-delims / ":" / "@"
    lit!(pchar, any!(unreserved!(), pct_encoded!(), sub_delims!(), "[:@]"));
    lit!(pchar_no_dot, any!(unreserved_no_dot!(), pct_encoded!(), sub_delims!(), "[:@]"));

    // query         = *( pchar / "/" / "?" )
    lit!(query, zero_or_more!(any!(pchar!(), "[/?]")));

    // fragment      = *( pchar / "/" / "?" )
    lit!(fragment, zero_or_more!(any!(pchar!(), "[/?]")));

    // segment       = *pchar
    lit!(segment, zero_or_more!(pchar!()));

    // segment-nz    = 1*pchar
    lit!(segment_nz, one_or_more!(pchar!()));
    lit!(segment_nz_no_leading_dot, group!(pchar_no_dot!(), zero_or_more!(pchar!())));

    // segment-nz-nc = 1*( unreserved / pct-encoded / sub-delims / "@" )
    //   ; non-zero-length segment without any colon ":"
    lit!(segment_nz_nc, one_or_more!(any!(unreserved!(), pct_encoded!(), sub_delims!(), "@")));

    // path-noscheme = segment-nz-nc *( "/" segment )
    lit!(path_noscheme, concat!(segment_nz_nc!(), zero_or_more_group!("/", segment!())));

    // path-rootless = segment-nz *( "/" segment )
    lit!(path_rootless, concat!(segment_nz!(), zero_or_more_group!("/", segment!())));
    lit!(
        path_rootless_no_leading_dot,
        concat!(segment_nz_no_leading_dot!(), zero_or_more_group!("/", segment!()))
    );

    // scheme        = ALPHA *( ALPHA / DIGIT / "+" / "-" / "." )
    //
    // This definition is not actually used in the proptest regular
    // expressions.
    //
    //   lit!(scheme, concat!(ALPHA!(), zero_or_more!(any!(ALPHA!(), DIGIT!(), "[-+.]"))));

    // Special scheme's require a domain authority (except "file", which
    // does not require a domain).
    lit!(
        special_scheme_not_file,
        any!(group!("ftp"), group!("http"), group!("https"), group!("ws"), group!("wss"),)
    );

    // ABSOLUTE_COMPONENT_URL requires a path. This is a variant of
    // hier-part that includes a path. Special scheme's (except "file") also
    // require a domain. If there is no authority (no host), then there must
    // be a non-empty path. A ".", and possibly a ".." can be interpreted
    // as an empty path, so to help proptest avoid constructing an invalid
    // hier_part, a leading dot will not be allowed.
    lit!(
        hier_part_with_path,
        any!(
            group!("//", authority!(), "/", path_rootless!()),
            group!(optional!("/"), path_rootless_no_leading_dot!())
        )
    );

    lit!(hier_part_with_authority_and_path, group!("//", authority!(), "/", path_rootless!()));

    // The formal definition of URI does not meet the requirements of
    // ABSOLUTE_COMPONENT_URL. With URI unused, the formal definitions of
    // hier-part, path-absolute, path-rootless, and path-empty are unused.
    //
    // path-abempty  = *( "/" segment )
    //
    //   lit!(path_abempty, zero_or_more_group!("/", segment!()));
    //
    // path-absolute = "/" [ segment-nz *( "/" segment ) ]
    //
    //   lit!(
    //       path_absolute,
    //       concat!("/", optional!(group!(segment_nz!(), zero_or_more_group!("/", segment!()))))
    //   );
    //
    // path-empty    = 0<pchar>
    //
    //   lit!(path_empty, repeat!(0, pchar!()));
    //
    // hier-part     = "//" authority path-abempty
    //               / path-absolute
    //               / path-rootless
    //               / path-empty
    //
    //   lit!(
    //       hier_part,
    //       any!(
    //           group!("//", authority!(), path_abempty!()),
    //           path_absolute!(),
    //           path_rootless!(),
    //           path_empty!(),
    //       )
    //   );
    //
    // URI           = scheme ":" hier-part [ "?" query ] [ "#" fragment ]
    //
    //   lit!(
    //       URI,
    //       concat!(
    //           scheme!(),
    //           ":",
    //           hier_part!(),
    //           optional_group!("\\?", query!()),
    //           optional_group!("#", fragment!())
    //       )
    //   );
}

lit!(
    ABSOLUTE_COMPONENT_URL,
    concat!(
        any!(
            group!(special_scheme_not_file!(), ":", hier_part_with_authority_and_path!(),),
            // Ideally, the next `any` option should be `scheme!()`:
            //   group!(scheme!(), ":", hier_part_with_path!(),),
            // but it should be scheme only if not
            // `special_scheme_not_file!()`. Since we can't instruct
            // proptest to generate a scheme that is NOT some specific
            // pattern, this will be a specific non-special scheme:
            group!("cast:", hier_part_with_path!(),),
        ),
        optional_group!("\\?", query!()),
        optional_group!("#", fragment!())
    )
);

lit!(
    SUBPACKAGED_COMPONENT_URL,
    concat!(
        path_noscheme!(),
        // query!() is not allowed in subpackaged component URLs

        // TODO(fxbug.dev/119726): fragments should be optional (as they are
        // for ABSOLUTE_COMPONENT_URL), but this is not possible until
        // `cm_types::Url::validate()` no longer returns an error when
        // a fragment is not included with a relative path (subpackaged)
        // component URL.
        //
        //   optional_group!("#", fragment!()),
        group!("#", fragment!()),
    )
);

lit!(FRAGMENT_ONLY_COMPONENT_URL, concat!("#", fragment!()));

lazy_static! {
    static ref A_BASE_URL: Url = Url::parse("relative:///").unwrap();
}

proptest! {
    #[test]
    fn check_absolute_url_matches_regex(s in ABSOLUTE_COMPONENT_URL!()) {
        if s.len() < MAX_URL_LENGTH {
            let mut errors = vec![];
            // NOTE: IF THIS TEST FLAKES AGAIN WITH `IdnaError`, consider
            // calling `Url::parse(&s)`, and if the result is `Err(IdnaError)`,
            // ignore the value (do not call `check_url()`).
            let _ = check_url(Some(&s), "", "", &mut errors);
            if !errors.is_empty() {
                println!("Error parsing URL: {s}");
            }
            for error in errors.iter() {
                println!("  {error:?}\n    Url::parse() returned: {:?}", Url::parse(&s));
            }
            prop_assert!(errors.is_empty());
        }
    }

    #[test]
    fn check_subpackage_url_matches_regex(s in SUBPACKAGED_COMPONENT_URL!()) {
        if s.len() < MAX_URL_LENGTH {
            let mut errors = vec![];
            let _ = check_url(Some(&s), "", "", &mut errors);
            if !errors.is_empty() {
                println!("Error parsing URL: {s}");
            }
            for error in errors.iter() {
                println!("  {error:?}\n    Url::parse() returned: {:?}", A_BASE_URL.join(&s));
            }
            prop_assert!(errors.is_empty());
        }
    }

    #[test]
    fn check_fragment_only_url_matches_regex(s in FRAGMENT_ONLY_COMPONENT_URL!()) {
        if s.len() < MAX_URL_LENGTH {
            let mut errors = vec![];
            let _ = check_url(Some(&s), "", "", &mut errors);
            if !errors.is_empty() {
                println!("Error parsing URL: {s}");
            }
            for error in errors.iter() {
                println!("  {error:?}\n    Url::parse() returned: {:?}", A_BASE_URL.join(&s));
            }
            prop_assert!(errors.is_empty());
        }
    }
}
