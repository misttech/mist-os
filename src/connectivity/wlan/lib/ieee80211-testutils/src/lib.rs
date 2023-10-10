// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use lazy_static::lazy_static;
use regex::Regex;

// The following regular expressions match a strict subset of the strings
// that match the regular expression defined in //src/developer/forensics/utils/redact/replacer.cc.
lazy_static! {
    pub static ref BSSID_REGEX: Regex =
        Regex::new(r#"^(?:[0-9a-fA-F]{2}:){5}[0-9a-fA-F]{2}$"#).unwrap();
    pub static ref SSID_REGEX: Regex = Regex::new(r#"^<ssid-[0-9a-fA-F]{0,64}>$"#).unwrap();
    pub static ref SSID_HASH_REGEX: Regex = Regex::new(r#"^[0-9a-fA-F]*$"#).unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;
    use fidl_fuchsia_wlan_ieee80211 as fidl_ieee80211;
    use ieee80211::Ssid;
    use rand::Rng;

    #[test]
    fn ssid_to_string_shape() {
        let mut rng = rand::thread_rng();
        let ssid = Ssid::from_bytes_unchecked(
            (0..(rng.gen::<usize>() % fidl_ieee80211::MAX_SSID_BYTE_LEN as usize))
                .map(|_| rng.gen::<u8>())
                .collect::<Vec<u8>>(),
        );
        assert!(SSID_REGEX.is_match(&ssid.to_string()));
    }
}
