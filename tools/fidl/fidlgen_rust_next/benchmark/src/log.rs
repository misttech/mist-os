// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::{generate_vec, make_rng, Generate};
use {fidl_next_test_benchmark as ftb_next, fidl_test_benchmark as ftb};

use rand::Rng;

impl_generate! {
    for ftb::Address, ftb_next::Address => rng {
        Self { x0: rng.gen(), x1: rng.gen(), x2: rng.gen(), x3: rng.gen() }
    }
}

const USERID: [&str; 9] =
    ["-", "alice", "bob", "carmen", "david", "eric", "frank", "george", "harry"];
const MONTHS: [&str; 12] =
    ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];
const TIMEZONE: [&str; 25] = [
    "-1200", "-1100", "-1000", "-0900", "-0800", "-0700", "-0600", "-0500", "-0400", "-0300",
    "-0200", "-0100", "+0000", "+0100", "+0200", "+0300", "+0400", "+0500", "+0600", "+0700",
    "+0800", "+0900", "+1000", "+1100", "+1200",
];
fn generate_date(rng: &mut impl Rng) -> String {
    format!(
        "{}/{}/{}:{}:{}:{} {}",
        rng.gen_range(1..=28),
        MONTHS[rng.gen_range(0..12)],
        rng.gen_range(1970..=2021),
        rng.gen_range(0..24),
        rng.gen_range(0..60),
        rng.gen_range(0..60),
        TIMEZONE[rng.gen_range(0..25)],
    )
}

const CODES: [u16; 63] = [
    100, 101, 102, 103, 200, 201, 202, 203, 204, 205, 206, 207, 208, 226, 300, 301, 302, 303, 304,
    305, 306, 307, 308, 400, 401, 402, 403, 404, 405, 406, 407, 408, 409, 410, 411, 412, 413, 414,
    415, 416, 417, 418, 421, 422, 423, 424, 425, 426, 428, 429, 431, 451, 500, 501, 502, 503, 504,
    505, 506, 507, 508, 510, 511,
];
const METHODS: [&str; 5] = ["GET", "POST", "PUT", "UPDATE", "DELETE"];
const ROUTES: [&str; 7] = [
    "/favicon.ico",
    "/css/index.css",
    "/css/font-awsome.min.css",
    "/img/logo-full.svg",
    "/img/splash.jpg",
    "/api/login",
    "/api/logout",
];
const PROTOCOLS: [&str; 4] = ["HTTP/1.0", "HTTP/1.1", "HTTP/2", "HTTP/3"];
fn generate_request(rng: &mut impl Rng) -> String {
    format!(
        "{} {} {}",
        METHODS[rng.gen_range(0..5)],
        ROUTES[rng.gen_range(0..7)],
        PROTOCOLS[rng.gen_range(0..4)],
    )
}

impl_generate! {
    for ftb::Log, ftb_next::Log => rng {
        Self {
            address: Generate::generate(rng),
            identity: "-".into(),
            userid: USERID[rng.gen_range(0..USERID.len())].into(),
            date: generate_date(rng),
            request: generate_request(rng),
            code: CODES[rng.gen_range(0..CODES.len())],
            size: rng.gen_range(0..100_000_000),
        }
    }
}

pub fn generate_input_rust(input_size: usize) -> ftb::Logs {
    let mut rng = make_rng();
    ftb::Logs { logs: generate_vec(&mut rng, input_size) }
}

pub fn generate_input_rust_next(input_size: usize) -> ftb_next::Logs {
    let mut rng = make_rng();
    ftb_next::Logs { logs: generate_vec(&mut rng, input_size) }
}
