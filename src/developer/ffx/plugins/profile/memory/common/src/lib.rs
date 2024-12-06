// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

/// Returns the name of a VMO category when the name match on of the rules.
/// This is used for presentation and aggregation.
pub fn vmo_name_to_digest_name(name: &str) -> &str {
    /// Default, global regex match.
    static RULES: std::sync::LazyLock<[(regex::Regex, &'static str); 13]> =
        std::sync::LazyLock::new(|| {
            [
                (
                    regex::Regex::new("ld\\.so\\.1-internal-heap|(^stack: msg of.*)").unwrap(),
                    "[process-bootstrap]",
                ),
                (regex::Regex::new("^blob-[0-9a-f]+$").unwrap(), "[blobs]"),
                (regex::Regex::new("^inactive-blob-[0-9a-f]+$").unwrap(), "[inactive blobs]"),
                (
                    regex::Regex::new("^thrd_t:0x.*|initial-thread|pthread_t:0x.*$").unwrap(),
                    "[stacks]",
                ),
                (regex::Regex::new("^data[0-9]*:.*$").unwrap(), "[data]"),
                (regex::Regex::new("^bss[0-9]*:.*$").unwrap(), "[bss]"),
                (regex::Regex::new("^relro:.*$").unwrap(), "[relro]"),
                (regex::Regex::new("^$").unwrap(), "[unnamed]"),
                (regex::Regex::new("^scudo:.*$").unwrap(), "[scudo]"),
                (regex::Regex::new("^.*\\.so.*$").unwrap(), "[bootfs-libraries]"),
                (regex::Regex::new("^stack_and_tls:.*$").unwrap(), "[bionic-stack]"),
                (regex::Regex::new("^ext4!.*$").unwrap(), "[ext4]"),
                (regex::Regex::new("^dalvik-.*$").unwrap(), "[dalvik]"),
            ]
        });
    RULES.iter().find(|(regex, _)| regex.is_match(name)).map_or(name, |rule| rule.1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rename_test() {
        pretty_assertions::assert_eq!(
            vmo_name_to_digest_name("ld.so.1-internal-heap"),
            "[process-bootstrap]"
        );
        pretty_assertions::assert_eq!(
            vmo_name_to_digest_name("stack: msg of 123"),
            "[process-bootstrap]"
        );
        pretty_assertions::assert_eq!(vmo_name_to_digest_name("blob-123"), "[blobs]");
        pretty_assertions::assert_eq!(vmo_name_to_digest_name("blob-15e0da8e"), "[blobs]");
        pretty_assertions::assert_eq!(
            vmo_name_to_digest_name("inactive-blob-123"),
            "[inactive blobs]"
        );
        pretty_assertions::assert_eq!(vmo_name_to_digest_name("thrd_t:0x123"), "[stacks]");
        pretty_assertions::assert_eq!(vmo_name_to_digest_name("initial-thread"), "[stacks]");
        pretty_assertions::assert_eq!(vmo_name_to_digest_name("pthread_t:0x123"), "[stacks]");
        pretty_assertions::assert_eq!(vmo_name_to_digest_name("data456:"), "[data]");
        pretty_assertions::assert_eq!(vmo_name_to_digest_name("bss456:"), "[bss]");
        pretty_assertions::assert_eq!(vmo_name_to_digest_name("relro:foobar"), "[relro]");
        pretty_assertions::assert_eq!(vmo_name_to_digest_name(""), "[unnamed]");
        pretty_assertions::assert_eq!(vmo_name_to_digest_name("scudo:primary"), "[scudo]");
        pretty_assertions::assert_eq!(vmo_name_to_digest_name("libfoo.so.1"), "[bootfs-libraries]");
        pretty_assertions::assert_eq!(vmo_name_to_digest_name("foobar"), "foobar");
        pretty_assertions::assert_eq!(
            vmo_name_to_digest_name("stack_and_tls:2331"),
            "[bionic-stack]"
        );
        pretty_assertions::assert_eq!(vmo_name_to_digest_name("ext4!foobar"), "[ext4]");
        pretty_assertions::assert_eq!(vmo_name_to_digest_name("dalvik-data1234"), "[dalvik]");
    }
}
