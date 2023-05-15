// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Message encode/decode helpers

use anyhow::Error;

/// Context derived from initial overnet handshake.
#[derive(Debug, Copy, Clone, PartialEq, Eq)]
pub struct Context {
    // If enabled, FIDL byte are framed with the persistence header.
    pub use_persistent_header: bool,
}

pub const DEFAULT_CONTEXT: Context = Context { use_persistent_header: false };

/// Decode some bytes into a FIDL type
pub fn decode_fidl_with_context<T: fidl::Persistable>(
    ctx: Context,
    bytes: &mut [u8],
) -> Result<T, Error> {
    if ctx.use_persistent_header {
        fidl::unpersist(bytes).map_err(Into::into)
    } else {
        let mut value = T::new_empty();
        // WARNING: Since we are decoding without a transaction header, we have to
        // provide a context manually. This could cause problems in future FIDL wire
        // format migrations, which are driven by header flags.
        let context =
            fidl::encoding::Context { wire_format_version: fidl::encoding::WireFormatVersion::V1 };
        fidl::encoding::Decoder::decode_with_context::<T>(context, bytes, &mut [], &mut value)?;
        Ok(value)
    }
}

/// Encode a FIDL type into some bytes
pub fn encode_fidl_with_context<'a, T: fidl::Persistable>(
    ctx: Context,
    value: &'a mut T,
) -> Result<Vec<u8>, Error>
where
    &'a T: fidl::encoding::Encode<T>,
{
    if ctx.use_persistent_header {
        fidl::persist(value).map_err(Into::into)
    } else {
        let (mut bytes, mut handles) = (Vec::new(), Vec::new());
        fidl::encoding::Encoder::encode_with_context::<T>(
            fidl::encoding::Context { wire_format_version: fidl::encoding::WireFormatVersion::V1 },
            &mut bytes,
            &mut handles,
            &*value,
        )?;
        assert_eq!(handles.len(), 0);
        Ok(bytes)
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use fidl_test_coding::Foo;

    #[fuchsia::test]
    fn encode_decode_without_persistent_header() {
        let coding_context = Context { use_persistent_header: false };
        let mut bytes =
            encode_fidl_with_context(coding_context, &mut Foo { byte: 5 }).expect("encoding fails");
        let result: Foo =
            decode_fidl_with_context(coding_context, &mut bytes).expect("decoding fails");
        assert_eq!(result.byte, 5);
    }

    #[fuchsia::test]
    fn encode_decode_with_persistent_header() {
        let coding_context = Context { use_persistent_header: true };
        let mut bytes =
            encode_fidl_with_context(coding_context, &mut Foo { byte: 5 }).expect("encoding fails");
        let result: Foo =
            decode_fidl_with_context(coding_context, &mut bytes).expect("decoding fails");
        assert_eq!(result.byte, 5);
    }
}
