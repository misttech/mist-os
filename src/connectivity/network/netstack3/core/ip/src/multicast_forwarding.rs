// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! An implementation of multicast forwarding.
//!
//! Multicast forwarding is the ability for netstack to forward multicast
//! packets that arrive on an interface out multiple interfaces (while also
//! optionally delivering the packet to the host itself if the arrival host has
//! an interest in the packet).
//!
//! Note that multicast forwarding decisions are made by consulting the
//! multicast routing table, a routing table entirely separate from the unicast
//! routing table(s).

pub(crate) mod api;
pub(crate) mod route;
pub(crate) mod state;
