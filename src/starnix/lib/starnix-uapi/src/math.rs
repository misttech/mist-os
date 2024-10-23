// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use super::errors::{errno, Errno};

pub fn round_up_to_increment<N, M>(size: N, increment: M) -> Result<N, Errno>
where
    N: TryInto<usize>,
    N: TryFrom<usize>,
    M: TryInto<usize>,
{
    let size: usize = size.try_into().map_err(|_| errno!(EINVAL))?;
    let increment: usize = increment.try_into().map_err(|_| errno!(EINVAL))?;
    let spare = size % increment;
    let result = if spare > 0 {
        size.checked_add(increment - spare).ok_or_else(|| errno!(EINVAL))?
    } else {
        size
    };
    N::try_from(result).map_err(|_| errno!(EINVAL))
}
