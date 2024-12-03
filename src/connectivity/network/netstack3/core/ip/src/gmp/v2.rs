// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! GMP v2 common implementation.
//!
//! GMPv2 is the common implementation of a fictitious GMP protocol that covers
//! the common parts of MLDv2 ([RFC 3810]) and IGMPv3 ([RFC 3376]).
//!
//! [RFC 3810]: https://datatracker.ietf.org/doc/html/rfc3810
//! [RFC 3376]: https://datatracker.ietf.org/doc/html/rfc3376

#[cfg_attr(test, derive(Debug))]
pub(super) struct GroupState;

impl GroupState {
    pub(super) fn new_for_mode_transition() -> Self {
        Self
    }
}
