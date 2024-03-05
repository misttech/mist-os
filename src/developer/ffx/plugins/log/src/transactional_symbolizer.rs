// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Error;
use std::borrow::Cow;

/// Represents a transactional message parsed
/// from the symbolizer output stream.
#[derive(Debug, PartialEq)]
struct Message<'a> {
    /// Transaction ID
    transaction_id: Option<u64>,
    /// Message, which may be empty
    message: Cow<'a, str>,
    /// Whether or not this is a commit message.
    is_txn_commit: bool,
}

fn parse_message<'a>(message: &'a str) -> Result<Message<'a>, Error> {
    let parts = message.split(':');
    if parts.count() < 3 || !message.starts_with("TXN") {
        return Ok(Message {
            transaction_id: None,
            message: Cow::Borrowed(message),
            is_txn_commit: false,
        });
    }
    let mut parts = message.split(':');
    // Read header
    let header = parts.next();
    // Read TXN ID
    // SAFETY: This unwrap is safe because we've verified that there are 3 parts
    // to the message above.
    let txn_id = parts.next().unwrap().parse::<u64>()?;
    Ok(Message {
        transaction_id: Some(txn_id),
        message: "".into(),
        is_txn_commit: header == Some("TXN-COMMIT"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;

    #[fuchsia::test]
    async fn transaction_parser_test() {
        // Invalid transaction
        assert_matches!(parse_message("TXN:notaninteger:this is not valid"), Err(_));
        // Valid transaction with ignored part. Messages should be on their own line,
        // because they might contain symbolizer markup.
        assert_eq!(
            parse_message("TXN:42:ignored").unwrap(),
            Message {
                is_txn_commit: false,
                message: Cow::Owned("".to_string()),
                transaction_id: Some(42),
            }
        );
        // Commit transaction, which indicates the entire message
        // has been handled by the symbolizer.
        assert_eq!(
            parse_message("TXN-COMMIT:42:ignored").unwrap(),
            Message {
                is_txn_commit: true,
                message: Cow::Owned("".to_string()),
                transaction_id: Some(42),
            }
        );
        // Message, which might either be symbolizer markup, or just
        // our own message
        assert_eq!(
            parse_message("not ignored, possible symbolizer markup").unwrap(),
            Message {
                is_txn_commit: false,
                message: Cow::Owned("not ignored, possible symbolizer markup".to_string()),
                transaction_id: None,
            }
        );

        // Message where the symbolizer markup starts with TXN: for some reason
        // but is not actually a transaction.
        assert_eq!(
            parse_message("TXN:").unwrap(),
            Message {
                is_txn_commit: false,
                message: Cow::Owned("TXN:".to_string()),
                transaction_id: None,
            }
        );
    }
}
