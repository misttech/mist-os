// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use crate::prelude_internal::*;

/// This structure represents DNS-SD server counters.
///
/// Functional equivalent of [`otsys::otDnssdCounters`](crate::otsys::otDnssdCounters).
#[derive(Debug, Default, Clone)]
#[repr(transparent)]
pub struct DnssdCounters(pub otDnssdCounters);

impl_ot_castable!(DnssdCounters, otDnssdCounters);

impl DnssdCounters {
    /// The number of successful responses.
    pub fn success_response(&self) -> u32 {
        self.0.mSuccessResponse
    }

    /// The number of server failure responses.
    pub fn server_failure_response(&self) -> u32 {
        self.0.mServerFailureResponse
    }

    /// The number of format error responses.
    pub fn format_error_response(&self) -> u32 {
        self.0.mFormatErrorResponse
    }

    /// The number of name error responses.
    pub fn name_error_response(&self) -> u32 {
        self.0.mNameErrorResponse
    }

    /// The number of 'not implemented' responses.
    pub fn not_implemented_response(&self) -> u32 {
        self.0.mNotImplementedResponse
    }

    /// The number of other responses.
    pub fn other_response(&self) -> u32 {
        self.0.mOtherResponse
    }

    /// The number of queries completely resolved by the local SRP server.
    pub fn resolved_by_srp(&self) -> u32 {
        self.0.mResolvedBySrp
    }
}
