// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#[macro_use]
extern crate lazy_static;

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::engine::Engine as _;

static CERT_PATH: &'static str = "/config/ssl/cert.pem";

lazy_static! {
    // To meet the required lifetime constraints we need to chain together
    // a series of statics.
    static ref RAW_DATA: String = {
        // I could have an environment variable override this, but i'd want it keyed
        // on being a dev build so you couldn't harm trust in prod
        std::fs::read_to_string(CERT_PATH)
            .map_err(|e| log::error!("unable to find root certificate store: {}, {:?}", CERT_PATH, e))
            .unwrap()
    };
    static ref CERT_DERS: Vec<Vec<u8>> = {
        let lines: Vec<&str> = RAW_DATA
            .split('\n')
            .filter(|l| !l.is_empty() && !l.starts_with(':') && !l.starts_with('#'))
            .collect();
        let mut cert_ders = vec![];
        let mut i = 0;
        while i < lines.len() {
            let start = i;
            if lines[i] != "-----BEGIN CERTIFICATE-----" {
                panic!("Missing certificate prefix");
            }
            while i < lines.len() && lines[i] != "-----END CERTIFICATE-----" {
                i += 1;
            }
            if i == lines.len() {
                panic!("Missing certificate suffix");
            }
            let end = i;
            i += 1;
            let cert_base64 = &lines[start + 1..end].join("");

            let cert_bytes = BASE64_STANDARD.decode(cert_base64.as_bytes())
                .expect("Invalid base64 encoding in root store");
            cert_ders.push(cert_bytes);
        }
        cert_ders
    };
    pub static ref TLS_SERVER_ROOTS: Vec<webpki::TrustAnchor<'static>> = {
        CERT_DERS.iter().map(|cert_bytes| {
            webpki::TrustAnchor::try_from_cert_der(cert_bytes)
                .expect("Parsing root certificate failed")
        }).collect()
    };
}

#[cfg(test)]
mod test {
    #[test]
    fn test_load() {
        assert_ne!(crate::TLS_SERVER_ROOTS.len(), 0);
    }
}
