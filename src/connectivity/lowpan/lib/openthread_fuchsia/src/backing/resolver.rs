// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use openthread_sys::*;

#[no_mangle]
unsafe extern "C" fn otPlatDnsStartUpstreamQuery(
    _a_instance: *mut otInstance,
    _a_txn: *mut otPlatDnsUpstreamQuery,
    _a_query: *const otMessage,
) {
}

#[no_mangle]
unsafe extern "C" fn otPlatDnsCancelUpstreamQuery(
    _a_instance: *mut otInstance,
    _a_txn: *mut otPlatDnsUpstreamQuery,
) {
}
