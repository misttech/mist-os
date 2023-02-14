// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

// TODO(fxb/104019): ffx auto-imports these libraries, even though we don't need them.
use argh as _;
use ffx_core as _;

pub use package_tool::PackageArchiveExtractCommand;

pub type FfxPluginCommand = PackageArchiveExtractCommand;
