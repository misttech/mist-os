// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Media modules, organized into four categories:
//!
//! Sources implement the bt_a2dp::media_task interfaces to encode and send packets to a peer
//!  - Sources may require a stream_builder to generate audio in-band
//! Sinks implement the bt_a2dp::media_task interfaces to decode and present packets locally.
//!

pub mod inband_source;

pub mod player_sink;

pub mod player;

pub mod sources;
