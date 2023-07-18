// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use packet_encoding::{Decodable, Encodable};

use crate::error::{Error, PacketError};
use crate::header::{Header, HeaderIdentifier, SingleResponseMode};

#[derive(Clone, Debug, PartialEq)]
pub struct HeaderSet {
    // TODO(fxbug.dev/125070): The ordering of headers matters. Make this more efficient by
    // possibly storing Headers in a Vec<T> and maintaining a map of HeaderIdentifier -> index to
    // improve the time complexity of lookups and updates.
    // See OBEX 1.5 Section 3.4 for the ordering requirements for each Operation.
    headers: Vec<(HeaderIdentifier, Header)>,
}

impl HeaderSet {
    pub fn new() -> Self {
        Self { headers: Vec::new() }
    }

    #[cfg(test)]
    pub fn from_headers(headers: Vec<Header>) -> Result<Self, Error> {
        let mut set = Self::new();
        for header in headers {
            set.add(header)?;
        }
        Ok(set)
    }

    #[cfg(test)]
    pub fn from_header(header: Header) -> Result<Self, Error> {
        Self::from_headers(vec![header])
    }

    pub fn is_empty(&self) -> bool {
        self.headers.is_empty()
    }

    pub fn contains_header(&self, id: &HeaderIdentifier) -> bool {
        self.headers.iter().find(|(header_id, _)| header_id == id).is_some()
    }

    pub fn get(&self, id: &HeaderIdentifier) -> Option<&Header> {
        self.headers.iter().find(|(header_id, _)| header_id == id).map(|(_, header)| header)
    }

    #[cfg(test)]
    pub fn contains_headers(&self, ids: &Vec<HeaderIdentifier>) -> bool {
        for id in ids {
            if !self.contains_header(id) {
                return false;
            }
        }
        true
    }

    pub fn add(&mut self, header: Header) -> Result<(), Error> {
        let id = header.identifier();
        if self.contains_header(&id) {
            return Err(Error::Duplicate(id));
        }

        // TODO(fxbug.dev/125070): Store `header` in the spec-defined order. For now, it is stored
        // in the order it is added.
        self.headers.push((id, header));
        Ok(())
    }

    /// Attempts to combine `HeaderSet`s together by modifying the current collection. Returns Ok on
    /// success, Error otherwise.
    pub fn try_append(&mut self, other: HeaderSet) -> Result<(), Error> {
        for (_, header) in other.headers {
            self.add(header)?;
        }
        Ok(())
    }

    /// Removes and returns the payload for the Body Header from the set.
    /// If `final_` is set, then the EndOfBody header payload is returned.
    /// Returns Error if the expected Header is not present in the collection.
    pub fn remove_body(&mut self, final_: bool) -> Result<Vec<u8>, Error> {
        if final_ {
            let Some(Header::EndOfBody(end_of_body)) = self.remove(&HeaderIdentifier::EndOfBody) else {
                return Err(PacketError::data("missing end of body header").into());
            };
            Ok(end_of_body)
        } else {
            let Some(Header::Body(body)) = self.remove(&HeaderIdentifier::Body) else {
                return Err(PacketError::data("missing body header").into());
            };
            Ok(body)
        }
    }

    /// Removes and returns the specified Header from the collection. Returns None if the Header is
    /// not present.
    pub fn remove(&mut self, id: &HeaderIdentifier) -> Option<Header> {
        let Some(index) = self.headers.iter().position(|(idx, _)| id == idx) else {
            return None;
        };
        Some(self.headers.remove(index).1)
    }

    /// Attempts to add the `SingleResponseMode` Header to the current `HeaderSet`.
    /// `local` is the supported mode of the local transport.
    /// Returns the SingleResponseMode value that was added to the set on success, Error if it
    /// couldn't be added or there was an incompatibility between the current set and `local`
    /// preferences.
    pub fn try_add_srm(&mut self, local: SingleResponseMode) -> Result<SingleResponseMode, Error> {
        // The current set has a preference for SRM. Verify it is compatible with the `local` mode.
        if let Some(Header::SingleResponseMode(srm)) =
            self.get(&HeaderIdentifier::SingleResponseMode)
        {
            // Current set requests to enable SRM, but it is not supported locally.
            if *srm == SingleResponseMode::Enable && local != SingleResponseMode::Enable {
                return Err(Error::SrmNotSupported);
            }
            // Otherwise, the mode currently specified in the headers is compatible.
            return Ok(*srm);
        }

        // Current set has no preference - default to the `local` preference.
        if local == SingleResponseMode::Enable {
            self.add(SingleResponseMode::Enable.into())?;
        }
        Ok(local)
    }
}

impl Encodable for HeaderSet {
    type Error = PacketError;

    fn encoded_len(&self) -> usize {
        self.headers.iter().map(|(_, h)| h.encoded_len()).sum()
    }

    fn encode(&self, buf: &mut [u8]) -> Result<(), Self::Error> {
        if buf.len() < self.encoded_len() {
            return Err(PacketError::BufferTooSmall);
        }
        let mut start_idx = 0;
        for (_, header) in &self.headers {
            header.encode(&mut buf[start_idx..])?;
            start_idx += header.encoded_len();
        }

        Ok(())
    }
}

impl Decodable for HeaderSet {
    type Error = PacketError;

    fn decode(buf: &[u8]) -> Result<Self, Self::Error> {
        let mut headers = Self::new();
        let mut start_idx = 0;
        while start_idx < buf.len() {
            let header = Header::decode(&buf[start_idx..])?;
            start_idx += header.encoded_len();
            headers.add(header).map_err(|e| PacketError::data(format!("{e:?}")))?;
        }
        Ok(headers)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::header::SingleResponseMode;
    use assert_matches::assert_matches;

    #[fuchsia::test]
    fn add_duplicate_header_is_error() {
        let mut headers = HeaderSet::new();
        headers.add(Header::Count(123)).expect("can add header");
        assert!(headers.contains_header(&HeaderIdentifier::Count));
        assert_matches!(headers.add(Header::Count(100)), Err(Error::Duplicate(_)));
    }

    #[fuchsia::test]
    fn try_append_success() {
        let mut headers1 = HeaderSet::from_header(Header::Name("foo".into())).unwrap();
        let headers2 = HeaderSet::from_header(Header::Description("bar".into())).unwrap();
        let () = headers1.try_append(headers2).expect("valid headers");
        assert!(headers1.contains_header(&HeaderIdentifier::Name));
        assert!(headers1.contains_header(&HeaderIdentifier::Description));
    }

    #[fuchsia::test]
    fn try_append_error() {
        let mut headers1 = HeaderSet::from_header(Header::Name("foo".into())).unwrap();
        let headers2 = HeaderSet::from_header(Header::Name("bar".into())).unwrap();
        assert_matches!(headers1.try_append(headers2), Err(Error::Duplicate(_)));
    }

    #[fuchsia::test]
    fn remove_headers() {
        let mut headers =
            HeaderSet::from_headers(vec![Header::Count(123), Header::Name("123".into())]).unwrap();
        assert!(headers.contains_header(&HeaderIdentifier::Count));
        assert!(headers.contains_header(&HeaderIdentifier::Name));
        assert!(headers.remove(&HeaderIdentifier::Count).is_some());
        assert!(!headers.contains_header(&HeaderIdentifier::Count));
        assert!(headers.remove(&HeaderIdentifier::Count).is_none());
        assert!(headers.remove(&HeaderIdentifier::Name).is_some());
        assert!(!headers.contains_header(&HeaderIdentifier::Name));
    }

    #[fuchsia::test]
    fn remove_body_headers() {
        let mut headers =
            HeaderSet::from_headers(vec![Header::Body(vec![1]), Header::EndOfBody(vec![1, 2])])
                .unwrap();
        assert!(headers.contains_header(&HeaderIdentifier::Body));
        assert!(headers.contains_header(&HeaderIdentifier::EndOfBody));

        let eob = headers.remove_body(true).expect("end of body exists");
        assert_eq!(eob, vec![1, 2]);
        // Trying to get it again is an Error since it no longer exists in the collection.
        assert_matches!(headers.remove_body(true), Err(Error::Packet(PacketError::Data(_))));

        let b = headers.remove_body(false).expect("end of body exists");
        assert_eq!(b, vec![1]);
        // Trying to get it again is an Error since it no longer exists in the collection.
        assert_matches!(headers.remove_body(false), Err(Error::Packet(PacketError::Data(_))));

        // Body exists, but user wants EOB.
        let mut headers = HeaderSet::from_headers(vec![Header::Body(vec![1])]).unwrap();
        assert_matches!(headers.remove_body(true), Err(Error::Packet(PacketError::Data(_))));

        // EOB exists, but user wants Body.
        let mut headers = HeaderSet::from_headers(vec![Header::EndOfBody(vec![1])]).unwrap();
        assert_matches!(headers.remove_body(false), Err(Error::Packet(PacketError::Data(_))));
    }

    #[fuchsia::test]
    fn try_add_srm_success() {
        // Trying to add SRM when it is supported locally should add the SRM Header to the
        // collection.
        let mut headers = HeaderSet::new();
        let result = headers.try_add_srm(SingleResponseMode::Enable).expect("can add SRM");
        assert_eq!(result, SingleResponseMode::Enable);
        assert_matches!(
            headers.get(&HeaderIdentifier::SingleResponseMode),
            Some(Header::SingleResponseMode(SingleResponseMode::Enable))
        );
        // Trying to add SRM when it isn't supported locally shouldn't add the SRM to the
        // collection.
        let mut headers = HeaderSet::new();
        let result = headers.try_add_srm(SingleResponseMode::Disable).expect("can add SRM");
        assert_eq!(result, SingleResponseMode::Disable);
        assert_matches!(headers.get(&HeaderIdentifier::SingleResponseMode), None);
        // Trying to add SRM when it is already enabled in the collection and is supported locally
        // should be a no-op.
        let mut headers = HeaderSet::from_header(SingleResponseMode::Enable.into()).unwrap();
        let result = headers.try_add_srm(SingleResponseMode::Enable).expect("can add SRM");
        assert_eq!(result, SingleResponseMode::Enable);
        assert_matches!(
            headers.get(&HeaderIdentifier::SingleResponseMode),
            Some(Header::SingleResponseMode(SingleResponseMode::Enable))
        );
        // Trying to add SRM when it already disabled in the collection and isn't supported locally
        // should be a no-op.
        let mut headers = HeaderSet::from_header(SingleResponseMode::Disable.into()).unwrap();
        let result = headers.try_add_srm(SingleResponseMode::Disable).expect("can add SRM");
        assert_eq!(result, SingleResponseMode::Disable);
        assert_matches!(
            headers.get(&HeaderIdentifier::SingleResponseMode),
            Some(Header::SingleResponseMode(SingleResponseMode::Disable))
        );
        // Trying to add SRM when it already disabled in the collection and is supported locally
        // should default to disabled.
        let mut headers = HeaderSet::from_header(SingleResponseMode::Disable.into()).unwrap();
        let result = headers.try_add_srm(SingleResponseMode::Enable).expect("can add SRM");
        assert_eq!(result, SingleResponseMode::Disable);
        assert_matches!(
            headers.get(&HeaderIdentifier::SingleResponseMode),
            Some(Header::SingleResponseMode(SingleResponseMode::Disable))
        );
    }

    #[fuchsia::test]
    fn try_add_srm_error() {
        // Trying to add SRM when it already enabled in the collection and isn't supported locally
        // is an Error. The collection itself is not modified.
        let mut headers = HeaderSet::from_header(SingleResponseMode::Enable.into()).unwrap();
        let result = headers.try_add_srm(SingleResponseMode::Disable);
        assert_matches!(result, Err(Error::SrmNotSupported));
        assert_matches!(
            headers.get(&HeaderIdentifier::SingleResponseMode),
            Some(Header::SingleResponseMode(SingleResponseMode::Enable))
        );
    }

    #[fuchsia::test]
    fn encode_header_set() {
        let headers = HeaderSet::from_headers(vec![
            Header::ConnectionId(1),
            Header::EndOfBody(vec![1, 2, 3]),
        ])
        .expect("can build header set");

        // The total length should be the sum of the lengths of each Header in the set.
        assert_eq!(headers.encoded_len(), 11);
        let mut buf = vec![0; headers.encoded_len()];
        headers.encode(&mut buf[..]).expect("can encode headers");
        // TODO(fxbug.dev/125070): Potentially update this check when `HeaderSet` enforces ordering
        // during encoding.
        let expected_buf = [0xcb, 0x00, 0x00, 0x00, 0x01, 0x49, 0x00, 0x06, 0x01, 0x02, 0x03];
        assert_eq!(buf, expected_buf);
    }

    #[fuchsia::test]
    fn decode_header_set() {
        let buf = [
            0x05, 0x00, 0x09, 0x00, 0x68, 0x00, 0x65, 0x00,
            0x00, // Description = "he" (String)
            0xd6, 0x00, 0x00, 0x00, 0x05, // Permissions = 5 (u32)
            0x97, 0x01, // SRM = Enabled = 1 (u8)
        ];
        let headers = HeaderSet::decode(&buf[..]).expect("can decode into headers");
        let expected_body = Header::Description("he".into());
        let expected_permissions = Header::Permissions(5);
        let expected_srm = Header::SingleResponseMode(SingleResponseMode::Enable);
        let expected_headers =
            HeaderSet::from_headers(vec![expected_body, expected_permissions, expected_srm])
                .unwrap();
        assert_eq!(headers, expected_headers);
    }

    #[fuchsia::test]
    fn decode_partial_header_set_error() {
        // Decoding should fail if one of the headers is invalidly formatted.
        let buf = [
            0xd6, 0x00, 0x00, 0x00, 0x09, // Permissions = 9 (u32)
            0x97, 0x01, // SRM = 1 (u8)
            0xc4, 0x00, // Time4Byte but missing remaining bytes.
        ];
        let headers = HeaderSet::decode(&buf[..]);
        assert_matches!(headers, Err(PacketError::BufferTooSmall));
    }
}
