// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub trait MacFmt {
    fn to_mac_string(&self) -> String;
    fn to_oui_uppercase(&self, delim: &str) -> String;
}

impl MacFmt for [u8; 6] {
    fn to_mac_string(&self) -> String {
        format!(
            "{:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x}",
            self[0], self[1], self[2], self[3], self[4], self[5]
        )
    }

    fn to_oui_uppercase(&self, sep: &str) -> String {
        format!("{:02X}{}{:02X}{}{:02X}", self[0], sep, self[1], sep, self[2])
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ieee80211::Bssid;
    use ieee80211_testutils::BSSID_REGEX;
    use rand::Rng;

    #[test]
    fn format_mac_str() {
        let mac: [u8; 6] = [0x00, 0x12, 0x48, 0x9a, 0xbc, 0xdf];
        assert_eq!(mac.to_mac_string(), "00:12:48:9a:bc:df");
    }

    #[fuchsia::test]
    fn test_printed_mac_format_exact() {
        let bssid = Bssid([0x01, 0x02, 0x01, 0x02, 0x01, 0x02]);
        assert_eq!("01:02:01:02:01:02", bssid.0.to_mac_string());
    }

    #[fuchsia::test]
    fn test_printed_mac_format_shape() {
        let mut rng = rand::thread_rng();
        let bssid = Bssid((0..6).map(|_| rng.gen::<u8>()).collect::<Vec<u8>>().try_into().unwrap());
        assert!(BSSID_REGEX.is_match(&bssid.0.to_mac_string()));
    }

    #[test]
    fn format_oui_uppercase() {
        let mac: [u8; 6] = [0x0a, 0xb1, 0xcd, 0x9a, 0xbc, 0xdf];
        assert_eq!(mac.to_oui_uppercase(""), "0AB1CD");
        assert_eq!(mac.to_oui_uppercase(":"), "0A:B1:CD");
        assert_eq!(mac.to_oui_uppercase("-"), "0A-B1-CD");
    }
}
