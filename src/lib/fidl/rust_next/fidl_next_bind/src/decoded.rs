// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use fidl_next_codec::Decoded;
use fidl_next_protocol::Transport;

use super::Method;

/// A decoded request.
pub type Request<
    M,
    #[cfg(feature = "fuchsia")] T = zx::Channel,
    #[cfg(not(feature = "fuchsia"))] T,
> = Decoded<<M as Method>::Request, <T as Transport>::RecvBuffer>;

/// A decoded response.
pub type Response<
    M,
    #[cfg(feature = "fuchsia")] T = zx::Channel,
    #[cfg(not(feature = "fuchsia"))] T,
> = Decoded<<M as Method>::Response, <T as Transport>::RecvBuffer>;
