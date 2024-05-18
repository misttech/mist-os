// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use futures::future::{FutureExt, LocalBoxFuture};
use futures::io::{AsyncWrite, AsyncWriteExt as _};
use futures::stream::StreamExt;
use playground::interpreter::Interpreter;
use playground::value::{PlaygroundValue, Value};
use std::collections::HashMap;
use std::io;

/// Given a list of values, determine if all of them are Value::Objects and if
/// they all have the same fields.
fn all_objects_same_fields(values: &[Value]) -> bool {
    if values.is_empty() {
        true
    } else if values.len() == 1 {
        matches!(&values[0], Value::Object(_))
    } else {
        values.windows(2).all(|arr| {
            let Value::Object(a) = &arr[0] else {
                return false;
            };
            let Value::Object(b) = &arr[1] else {
                return false;
            };

            if a.len() != b.len() {
                return false;
            }

            a.iter().zip(b.iter()).all(|((a, _), (b, _))| a == b)
        })
    }
}

/// Display a list of values which came as the result of a command in a pretty way.
async fn display_result_list<'a, W: AsyncWrite + Unpin + 'a>(
    writer: &'a mut W,
    values: Vec<Value>,
    interpreter: &'a Interpreter,
) -> io::Result<()> {
    if values.is_empty() {
        writer.write_all(format!("{}\n", Value::List(values)).as_bytes()).await?
    } else if values.iter().all(|x| matches!(x, Value::U8(_))) {
        let string = values
            .iter()
            .map(|x| {
                let Value::U8(x) = x else { unreachable!() };
                *x
            })
            .collect::<Vec<_>>();
        if let Ok(string) = String::from_utf8(string) {
            writer.write_all(format!("{string}\n").as_bytes()).await?;
        } else {
            writer.write_all(format!("{}\n", Value::List(values)).as_bytes()).await?
        }
    } else if all_objects_same_fields(&values) {
        let mut columns = HashMap::new();
        let mut column_order = Vec::new();
        let row_count = values.len();

        for value in values {
            let Value::Object(value) = value else { unreachable!() };

            if column_order.is_empty() {
                column_order = value.iter().map(|x| x.0.clone()).collect();
            }

            for (column, value) in value {
                let mut value_text = Vec::new();
                let mut cursor = futures::io::Cursor::new(&mut value_text);
                display_result(&mut cursor, true, Ok(value), interpreter).await?;
                let value_text = String::from_utf8(value_text).unwrap();
                columns.entry(column).or_insert(Vec::new()).push(value_text);
            }
        }

        let mut column_lengths: HashMap<_, _> = column_order
            .iter()
            .map(|column| {
                let max =
                    columns.get(column).unwrap().iter().map(|x| x.chars().count()).max().unwrap();
                let max = std::cmp::max(max, column.chars().count());
                (column, max)
            })
            .collect();

        if let Some(last_column) = column_order.last() {
            let last_len =
                column_lengths.get_mut(last_column).expect("Column should be in lengths table!");
            if *last_len == last_column.len() {
                *last_len += 1;
            }
        }

        let header = column_order
            .iter()
            .map(|column| {
                format!(" {column:<length$}", length = column_lengths.get(column).unwrap())
            })
            .collect::<Vec<_>>()
            .join("  ");
        writer.write_all(format!("\x1b[1;30;103m{header}\x1b[0m\n").as_bytes()).await?;

        for row in 0..row_count {
            let row = column_order
                .iter()
                .map(|column| {
                    let column_text = &columns.get(column).unwrap()[row];
                    format!(" {column_text:<length$}", length = column_lengths.get(column).unwrap())
                })
                .collect::<Vec<_>>()
                .join(" |");
            writer.write_all(format!("{row}\n").as_bytes()).await?;
        }
    } else {
        writer.write_all(format!("{}\n", Value::List(values)).as_bytes()).await?
    }

    Ok(())
}

/// Display a result from running a command in a prettified way.
pub fn display_result<'a, W: AsyncWrite + Unpin + 'a>(
    writer: &'a mut W,
    inline: bool,
    result: playground::error::Result<Value>,
    interpreter: &'a Interpreter,
) -> LocalBoxFuture<'a, io::Result<()>> {
    async move {
        match result {
            Ok(Value::OutOfLine(PlaygroundValue::Iterator(x))) if !inline => {
                let stream = interpreter.replayable_iterator_to_stream(x);
                let values = stream
                    .collect::<Vec<_>>()
                    .await
                    .into_iter()
                    .collect::<playground::error::Result<Vec<_>>>();
                match values {
                    Ok(values) => display_result_list(writer, values, interpreter).await?,
                    Err(other) => display_result(writer, inline, Err(other), interpreter).await?,
                }
            }
            Ok(Value::List(values)) if !inline => {
                display_result_list(writer, values, interpreter).await?
            }
            Ok(Value::String(s)) if !inline => {
                writer.write_all(format!("{s}\n").as_bytes()).await?
            }
            Ok(Value::String(s)) => writer.write_all(s.as_bytes()).await?,
            Ok(x) if !inline => writer.write_all(format!("{}\n", x).as_bytes()).await?,
            Ok(x) => writer.write_all(format!("{}", x).as_bytes()).await?,
            Err(x) if !inline => writer.write_all(format!("Err: {:#}\n", x).as_bytes()).await?,
            Err(x) => writer.write_all(format!("Err: {:#}", x).as_bytes()).await?,
        }
        Ok(())
    }
    .boxed_local()
}

#[cfg(test)]
mod test {
    use super::*;

    #[fuchsia::test]
    async fn test_table() {
        let mut got = Vec::<u8>::new();
        let ns = fidl_codec::library::Namespace::new();
        let (fs_root, _server) = fidl::endpoints::create_endpoints();
        let (interpreter, task) = playground::interpreter::Interpreter::new(ns, fs_root).await;
        fuchsia_async::Task::spawn(task).detach();

        display_result(
            &mut got,
            false,
            Ok(Value::List(vec![
                Value::Object(vec![
                    ("a".to_owned(), Value::U8(1)),
                    ("b".to_owned(), Value::String("Hi".to_owned())),
                ]),
                Value::Object(vec![
                    ("a".to_owned(), Value::U8(2)),
                    ("b".to_owned(), Value::String("Hello".to_owned())),
                ]),
                Value::Object(vec![
                    ("a".to_owned(), Value::U8(3)),
                    ("b".to_owned(), Value::String("Good Evening".to_owned())),
                ]),
            ])),
            &interpreter,
        )
        .await
        .unwrap();
        assert_eq!(b"\x1b[1;30;103m a     b           \x1b[0m\n 1u8 | Hi          \n 2u8 | Hello       \n 3u8 | Good Evening\n", got.as_slice());
    }

    #[fuchsia::test]
    async fn test_short_list() {
        let mut got = Vec::<u8>::new();
        let ns = fidl_codec::library::Namespace::new();
        let (fs_root, _server) = fidl::endpoints::create_endpoints();
        let (interpreter, task) = playground::interpreter::Interpreter::new(ns, fs_root).await;
        fuchsia_async::Task::spawn(task).detach();

        display_result(&mut got, false, Ok(Value::List(vec![Value::U32(2)])), &interpreter)
            .await
            .unwrap();
        assert_eq!(b"[ 2u32 ]\n", got.as_slice());
    }

    #[fuchsia::test]
    async fn test_short_data() {
        let mut got = Vec::<u8>::new();
        let ns = fidl_codec::library::Namespace::new();
        let (fs_root, _server) = fidl::endpoints::create_endpoints();
        let (interpreter, task) = playground::interpreter::Interpreter::new(ns, fs_root).await;
        fuchsia_async::Task::spawn(task).detach();

        display_result(
            &mut got,
            false,
            Ok(Value::List(vec![
                Value::Object(vec![
                    ("aa".to_owned(), Value::U8(1)),
                    ("bb".to_owned(), Value::U8(2)),
                ]),
                Value::Object(vec![
                    ("aa".to_owned(), Value::U8(2)),
                    ("bb".to_owned(), Value::U8(3)),
                ]),
                Value::Object(vec![
                    ("aa".to_owned(), Value::U8(3)),
                    ("bb".to_owned(), Value::U8(4)),
                ]),
            ])),
            &interpreter,
        )
        .await
        .unwrap();
        assert_eq!(
            b"\x1b[1;30;103m aa    bb \x1b[0m\n 1u8 | 2u8\n 2u8 | 3u8\n 3u8 | 4u8\n",
            got.as_slice()
        );
    }
}
