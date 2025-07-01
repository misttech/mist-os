// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use std::collections::HashSet;
use std::sync::LazyLock;

pub fn is_reserved(name: &str) -> bool {
    KEYWORDS.contains(name)
}

pub fn is_compat_reserved(name: &str) -> bool {
    is_reserved(name) || COMPAT_RESERVED_SUFFIX_LIST.iter().any(|suffix| name.ends_with(suffix))
}

static KEYWORDS: LazyLock<HashSet<String>> =
    LazyLock::new(|| KEYWORDS_LIST.iter().map(|k| k.to_string()).collect());

const KEYWORDS_LIST: &[&str] = &[
    "abstract",
    "as",
    "async",
    "await",
    "become",
    "box",
    "break",
    "const",
    "continue",
    "crate",
    "do",
    "dyn",
    "else",
    "enum",
    "extern",
    "false",
    "final",
    "fn",
    "for",
    "if",
    "impl",
    "in",
    "let",
    "loop",
    "macro",
    "macro_rules",
    "match",
    "mod",
    "move",
    "mut",
    "override",
    "pub",
    "priv",
    "ref",
    "return",
    "self",
    "Self",
    "static",
    "struct",
    "super",
    "trait",
    "true",
    "try",
    "type",
    "typeof",
    "unsafe",
    "unsized",
    "use",
    "virtual",
    "where",
    "while",
    "yield",
];

const COMPAT_RESERVED_SUFFIX_LIST: &[&str] =
    &["Impl", "Marker", "Proxy", "ProxyProtocol", "ControlHandle", "Responder", "Server"];
