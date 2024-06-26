// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

pub mod display_configuration;
pub mod display_controller;
mod display_fidl_handler;
pub mod types;

pub use display_configuration::build_display_default_settings;
