// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::Error;
use futures::Stream;
use pin_project::pin_project;
use std::{
    borrow::Cow,
    fmt::Display,
    mem::swap,
    ops::Deref,
    pin::Pin,
    task::{ready, Context, Poll},
};
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader, Lines};

/// Start of transaction message
const TRANSACTION_BEGIN_STR: &'static str = "TXN";

/// Replacement message to escape out TXN
const TRANSACTION_REPLACEMENT_STR: &'static str = "RTXN";

/// Represents a transactional message parsed
/// from the symbolizer output stream.
#[derive(Debug, PartialEq)]
struct Message<'a> {
    /// Transaction ID
    transaction_id: Option<u64>,
    /// Message, which may be empty
    message: EscapedMessage<'a>,
    /// Whether or not this is a commit message.
    is_txn_commit: bool,
}

#[derive(Debug, PartialEq, Clone)]
struct EscapedMessage<'a>(Cow<'a, str>);

#[derive(Debug, PartialEq, Clone)]
struct UnescapedMessage<'a>(Cow<'a, str>);

impl<'a> From<Cow<'a, str>> for EscapedMessage<'a> {
    fn from(value: Cow<'a, str>) -> Self {
        Self(value)
    }
}

impl<'a> From<&'a str> for EscapedMessage<'a> {
    fn from(value: &'a str) -> Self {
        Self(Cow::Borrowed(value))
    }
}

impl<'a> Deref for EscapedMessage<'a> {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<'a> Deref for UnescapedMessage<'a> {
    type Target = str;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<'a> Display for UnescapedMessage<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.0.fmt(f)
    }
}

impl<'a> From<Cow<'a, str>> for UnescapedMessage<'a> {
    fn from(value: Cow<'a, str>) -> Self {
        Self(value)
    }
}

impl<'a> From<&'a str> for UnescapedMessage<'a> {
    fn from(value: &'a str) -> Self {
        Self(Cow::Borrowed(value))
    }
}

impl<'a> PartialEq<&str> for UnescapedMessage<'a> {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

impl<'a> PartialEq<&str> for EscapedMessage<'a> {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

impl<'a> EscapedMessage<'a> {
    /// Unescapes a message received from the symbolizer. The string has to
    /// be modified when sending and receiving from the symbolizer in order
    /// to ensure that a string containing something that looks like a TXN
    /// isn't actually treated as a TXN unless it comes from us.
    fn unescape(self) -> UnescapedMessage<'a> {
        if !self.contains(TRANSACTION_REPLACEMENT_STR) {
            // Can't use Deref here as the Deref trait
            // doesn't preserve the incoming 'a lifetime,
            // and is instead bound by the anonymous lifetime of
            // the value passed in.
            self.0.into()
        } else {
            Cow::Owned::<'a, str>(self.replace(TRANSACTION_REPLACEMENT_STR, TRANSACTION_BEGIN_STR))
                .into()
        }
    }

    /// Parses this escaped message into a Message.
    fn parse(self) -> Result<Message<'a>, Error> {
        let parts = self.split(':');
        if parts.count() < 3 || !self.starts_with(TRANSACTION_BEGIN_STR) {
            return Ok(Message { transaction_id: None, message: self, is_txn_commit: false });
        }
        let mut parts = self.split(':');
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
}

impl<'a> UnescapedMessage<'a> {
    /// Sanitizes a message to be sent to the symbolizer. This is needed
    /// to ensure that strings starting with TXN: are not treated as a
    /// valid TXN if they are contained within a log message.
    /// We are being extra cautious here and replacing all instances of TXN
    /// with RTXN to prevent us from being susceiptible to Unicode-based
    /// attacks where certain Unicode sequences could look like newlines
    /// when viewed as bytes rather than as Unicode characters.
    /// It's safest to just replace all instances for this reason.
    fn sanitize(self) -> EscapedMessage<'a> {
        if !self.contains(TRANSACTION_BEGIN_STR) {
            // Can't use Deref here as the Deref trait
            // doesn't preserve the incoming 'a lifetime,
            // and is instead bound by the anonymous lifetime of
            // the value passed in.
            self.0.into()
        } else {
            Cow::Owned::<'a, str>(self.replace(TRANSACTION_BEGIN_STR, TRANSACTION_REPLACEMENT_STR))
                .into()
        }
    }
}

#[derive(thiserror::Error, Debug)]
pub enum ReadError {
    #[error("Mismatched transaction ID")]
    MismatchedTransaction,
    #[error(transparent)]
    UnknownError(#[from] anyhow::Error),
    #[error(transparent)]
    IoError(#[from] std::io::Error),
}

#[derive(Debug)]
enum TransactionState<'a> {
    /// Initial state
    Start,
    /// In transaction with ID and partial message
    InTransaction { transaction_id: u64, message: UnescapedMessage<'a>, is_first_message: bool },
}

impl<'a> Default for TransactionState<'a> {
    fn default() -> Self {
        Self::Start
    }
}

/// Transaction reader, which accepts input strings and outputs
/// transaction data.
#[derive(Default, Debug)]
struct TransactionReader<'a> {
    state: TransactionState<'a>,
}

impl<'a> TransactionReader<'a> {
    /// Pushes a line of text to the reader, returning information about a completed
    /// transaction if committed.
    fn push_line(&mut self, line: EscapedMessage<'a>) -> Result<Option<(u64, String)>, ReadError> {
        let msg = line.parse()?;
        let mut state = TransactionState::Start;
        swap(&mut state, &mut self.state);
        match (state, msg.is_txn_commit, msg.transaction_id) {
            // Not in transaction, no TXN ID to associate with
            (TransactionState::Start, false, None) | (TransactionState::Start, true, _) => {
                unreachable!("Unexpected message received before a valid transaction ID")
            }
            // Not in transaction, first message containing TXN ID
            (TransactionState::Start, false, Some(txn_id)) => {
                self.state = TransactionState::InTransaction {
                    transaction_id: txn_id,
                    message: msg.message.unescape().into(),
                    is_first_message: true,
                };
                Ok(None)
            }
            // Mismatched transaction IDs when commit is specified
            (TransactionState::InTransaction { transaction_id, .. }, true, Some(id))
                if id != transaction_id =>
            {
                Err(ReadError::MismatchedTransaction)
            }
            // Commit transaction
            (TransactionState::InTransaction { transaction_id, message, .. }, true, _) => {
                Ok(Some((transaction_id, (*message).into())))
            }
            // Inside transaction (first message)
            (
                TransactionState::InTransaction { transaction_id, message, is_first_message },
                false,
                _,
            ) => {
                let mut s = message.to_string();
                if !is_first_message {
                    s.push('\n');
                }
                s.push_str(&*msg.message.unescape());
                self.state = TransactionState::InTransaction {
                    transaction_id,
                    message: Cow::Owned::<'a, str>(s).into(),
                    is_first_message: false,
                };
                Ok(None)
            }
        }
    }
}

/// Asynchronous version of TransactionReader,
/// which can read from a symbolizer's stdout
/// and return transactions as they complete.
#[pin_project]
struct AsyncTransactionReader<T> {
    reader: TransactionReader<'static>,
    #[pin]
    lines: Lines<BufReader<T>>,
}

impl<T> AsyncTransactionReader<T>
where
    T: AsyncRead + Unpin,
{
    /// Creates a new async transaction reader in the reading state.
    fn new(stream: T) -> Self {
        let lines = BufReader::new(stream).lines();
        Self { reader: Default::default(), lines }
    }
}

impl<T> Stream for AsyncTransactionReader<T>
where
    T: AsyncRead + Unpin,
{
    type Item = Result<(u64, String), ReadError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.project();
        match ready!(this.lines.poll_next_line(cx)) {
            Ok(Some(line)) => {
                // Tokio's poll works different than futures::Stream.
                // It expects poll to be called continuously until it returns
                // Pending, rather than itself signalling the waker if more space
                // is available in the buffer.
                // If it returns a line, it's necessary to schedule another wakeup
                // so that additional lines are returned if they are available in the buffer.
                cx.waker().wake_by_ref();
                match this.reader.push_line(Cow::from(line).into()) {
                    Ok(Some(value)) => Poll::Ready(Some(Ok(value))),
                    Ok(None) => Poll::Pending,
                    Err(e) => Poll::Ready(Some(Err(e))),
                }
            }
            Err(err) => Poll::Ready(Some(Err(err.into()))),
            Ok(None) => Poll::Ready(None),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use assert_matches::assert_matches;
    use futures::StreamExt;
    use std::fmt::Write;

    #[fuchsia::test]
    async fn string_escape_test() {
        // Escaped string
        let input = "TXN:0:start\nHello world!\n\n\nTest line 2\nRTXN:0:nothing\nTXN-COMMIT:0:COMMIT\nTXN:1:start";
        // Escaped view of string
        let unescaped_input = UnescapedMessage::from(input);

        assert_eq!(unescaped_input.clone().sanitize(), "RTXN:0:start\nHello world!\n\n\nTest line 2\nRRTXN:0:nothing\nRTXN-COMMIT:0:COMMIT\nRTXN:1:start");
        // Roundtrip
        assert_eq!(unescaped_input.sanitize().unescape(), input);
        // No-op
        assert_eq!(UnescapedMessage::from("nop").sanitize(), "nop");
        assert_eq!(UnescapedMessage::from("nop").sanitize(), "nop");
    }

    #[fuchsia::test]
    async fn async_read_transacted_test() {
        let input = BufReader::new("TXN:0:start\nHello world!\n\n\nTest line 2\nRTXN:0:escaped message\nTXN-COMMIT:0:COMMIT\nTXN:1:second\nsecond transaction\nTXN-COMMIT:1:end\nTXN:1:start".as_bytes());
        let mut reader = AsyncTransactionReader::new(input);
        let (txn, msg) = assert_matches!(reader.next().await, Some(Ok(value)) => value);
        assert_eq!(msg, "Hello world!\n\n\nTest line 2\nTXN:0:escaped message");
        assert_eq!(txn, 0);
        let (txn, msg) = assert_matches!(reader.next().await, Some(Ok(value)) => value);
        assert_eq!(msg, "second transaction");
        assert_eq!(txn, 1);
        assert_matches!(reader.next().await, None);
    }

    #[fuchsia::test]
    async fn read_transacted_test() {
        let mut reader = TransactionReader::default();
        let stdout = "TXN:0:start\nHello world!\n\n\nTest line 2\nRTXN:0:nothing\nTXN-COMMIT:0:COMMIT\nTXN:1:start";
        let lines = stdout.lines();
        let mut output = String::new();
        for line in lines {
            if let Some((transaction_id, message)) = reader.push_line(line.into()).unwrap() {
                assert_eq!(transaction_id, 0);
                write!(output, "{message}").unwrap();
            }
        }
        assert_eq!(output, "Hello world!\n\n\nTest line 2\nTXN:0:nothing");
    }

    #[fuchsia::test]
    async fn transaction_parser_test() {
        // Invalid transaction
        assert_matches!(EscapedMessage::from("TXN:notaninteger:this is not valid").parse(), Err(_));
        // Valid transaction with ignored part. Messages should be on their own line,
        // because they might contain symbolizer markup.
        assert_matches!(
            EscapedMessage::from("TXN:42:ignored").parse(),
            Ok(msg) if msg == Message {
                is_txn_commit: false,
                message: "".into(),
                transaction_id: Some(42),
            }
        );
        // Commit transaction, which indicates the entire message
        // has been handled by the symbolizer.
        assert_matches!(
            EscapedMessage::from("TXN-COMMIT:42:ignored").parse(),
            Ok(msg) if msg == Message {
                is_txn_commit: true,
                message: "".into(),
                transaction_id: Some(42),
            }
        );
        // Message, which might either be symbolizer markup, or just
        // our own message
        assert_matches!(
            EscapedMessage::from("not ignored, possible symbolizer markup").parse(),
            Ok(msg) if msg == Message {
                is_txn_commit: false,
                message: "not ignored, possible symbolizer markup".into(),
                transaction_id: None,
            }
        );

        // Message where the symbolizer markup starts with TXN: for some reason
        // but is not actually a transaction.
        assert_matches!(
            EscapedMessage::from("TXN:").parse(),
            Ok(msg) if msg == Message {
                is_txn_commit: false,
                message: "TXN:".into(),
                transaction_id: None,
            }
        );
    }
}
