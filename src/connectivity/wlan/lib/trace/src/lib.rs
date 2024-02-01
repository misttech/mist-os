// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod names;

use {fuchsia_trace as trace, fuchsia_zircon as zx};

pub fn instant_wlancfg_start() {
    if let Some(context) = trace::TraceCategoryContext::acquire(names::CATEGORY_WLAN) {
        trace::instant(&context, names::NAME_WLANCFG_START, trace::Scope::Process, &[]);
    }
}

pub fn async_begin_wlansoftmac_tx(async_id: trace::Id, origin: &str) {
    trace::async_begin(
        async_id,
        names::CATEGORY_WLAN,
        names::NAME_WLANSOFTMAC_TX,
        &[trace::ArgValue::of("origin", origin)],
    );
}

pub fn async_end_wlansoftmac_tx(async_id: trace::Id, status: zx::Status) {
    trace::async_end(
        async_id,
        names::CATEGORY_WLAN,
        names::NAME_WLANSOFTMAC_TX,
        &[trace::ArgValue::of("status", format!("{}", status).as_str())],
    );
}
