// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use async_trait::async_trait;
use fidl::endpoints::{ClientEnd, Proxy};
use fidl_codec::library as lib;
use fidl_fuchsia_io as fio;
use std::future::Future;
use std::sync::Arc;
use vfs::directory::helper::DirectlyMutable;

use crate::interpreter::Interpreter;
use crate::value::{PlaygroundValue, Value, ValueExt};

/// Get an interpreter, optionally loaded up with the test FIDL info, and with
/// its background task handed over to the executor and polling.
async fn test_interpreter(
    with_fidl: bool,
    with_dirs: Option<ClientEnd<fio::DirectoryMarker>>,
) -> Interpreter {
    let mut ns = lib::Namespace::new();
    if with_fidl {
        ns.load(test_fidl::TEST_FIDL).unwrap();
        ns.load(test_fidl::FUCHSIA_IO_FIDL).unwrap();
    }
    let fs_root = with_dirs.unwrap_or_else(|| fidl::endpoints::create_endpoints().0);
    let (interpreter, fut) = Interpreter::new(ns, fs_root).await;
    fuchsia_async::Task::spawn(fut).detach();

    interpreter
}

/// A vfs directory entry for a simple symlink.
struct TestSymlink(String);

impl vfs::symlink::Symlink for TestSymlink {
    fn read_target(&self) -> impl Future<Output = Result<Vec<u8>, fidl::Status>> + Send {
        let got = self.0.as_bytes().to_vec();
        async move { Ok(got) }
    }
}

#[async_trait]
impl vfs::node::Node for TestSymlink {
    async fn get_attributes(
        &self,
        _requested_attributes: fio::NodeAttributesQuery,
    ) -> Result<fio::NodeAttributes2, fidl::Status> {
        unreachable!();
    }

    async fn get_attrs(&self) -> Result<fio::NodeAttributes, fidl::Status> {
        Ok(fio::NodeAttributes {
            mode: fio::MODE_TYPE_SYMLINK
                | vfs::common::rights_to_posix_mode_bits(
                    /*r*/ true, /*w*/ false, /*x*/ false,
                ),
            id: fio::INO_UNKNOWN,
            content_size: self.0.as_bytes().len() as u64,
            storage_size: self.0.as_bytes().len() as u64,
            link_count: 1,
            creation_time: 0,
            modification_time: 0,
        })
    }
}

/// A test string.
pub const NEILS_PHILOSOPHY: &'static [u8] =
    b"It's not about the walls, Max, it's about what's outside of them.";

impl vfs::directory::entry::DirectoryEntry for TestSymlink {
    fn entry_info(&self) -> vfs::directory::entry::EntryInfo {
        vfs::directory::entry::EntryInfo::new(fio::INO_UNKNOWN, fio::DirentType::Symlink)
    }

    fn open_entry(
        self: Arc<Self>,
        request: vfs::directory::entry::OpenRequest<'_>,
    ) -> Result<(), fidl::Status> {
        request.open_symlink(self)
    }
}

/// Helper for quickly testing a code snippet to see if it returns the correct
/// value.
pub struct Test<T> {
    test: T,
    with_fidl: bool,
    with_dirs: Option<fidl::endpoints::ClientEnd<fio::DirectoryMarker>>,
}

impl<T: AsRef<str>> Test<T> {
    /// Create a new test which will run the given Playground code.
    pub fn test(test: T) -> Self {
        Test { test, with_fidl: false, with_dirs: None }
    }

    /// Load the FIDL test data into the interpreter before this test runs.
    pub fn with_fidl(mut self) -> Self {
        self.with_fidl = true;
        self
    }

    /// Set `$fs_root` with a set of standard test directories.
    pub fn with_standard_test_dirs(mut self) -> Self {
        let simple = vfs::directory::mutable::simple();
        let proxy = vfs::directory::spawn_directory(Arc::clone(&simple));
        let test_subdir = vfs::directory::mutable::simple();
        let foo_subdir = vfs::directory::mutable::simple();
        let test_file = vfs::file::read_only(NEILS_PHILOSOPHY);
        foo_subdir
            .add_entry("relative_symlink", Arc::new(TestSymlink("../neils_philosophy".to_owned())))
            .unwrap();
        foo_subdir
            .add_entry("absolute_symlink", Arc::new(TestSymlink("/test".to_owned())))
            .unwrap();
        test_subdir.add_entry("foo", foo_subdir).unwrap();
        test_subdir.add_entry("neils_philosophy", test_file).unwrap();
        simple.add_entry("test", test_subdir).unwrap();
        assert!(
            self.with_dirs.replace(proxy.into_client_end().unwrap()).is_none(),
            "Set directory root twice!"
        );
        self
    }

    /// Run this test, check the output with the given closure.
    pub async fn check(self, eval: impl Fn(Value)) {
        eval(
            test_interpreter(self.with_fidl, self.with_dirs)
                .await
                .run(self.test.as_ref())
                .await
                .unwrap(),
        )
    }

    /// Run this test, check the output with the given closure, which may be a future.
    pub async fn check_async<F: std::future::Future<Output = ()>>(self, eval: impl Fn(Value) -> F) {
        eval(
            test_interpreter(self.with_fidl, self.with_dirs)
                .await
                .run(self.test.as_ref())
                .await
                .unwrap(),
        )
        .await
    }
}

#[fuchsia::test]
async fn add() {
    Test::test("2 + 2")
        .check(|value| {
            assert_eq!(4, value.try_usize().unwrap());
        })
        .await;
}

#[fuchsia::test]
async fn assignment() {
    Test::test("let a = 0; $a = 4; $a")
        .check(|value| {
            assert_eq!(4, value.try_usize().unwrap());
        })
        .await;
}

#[fuchsia::test]
async fn bare_string() {
    Test::test("def f x -> $x; f abc123")
        .check(|value| {
            let Value::String(value) = value else {
                panic!();
            };

            assert_eq!("abc123", &value);
        })
        .await;
}

#[fuchsia::test]
async fn block() {
    Test::test("let k = 2; { let k = 5; \"abc\"; $k + 3} * $k")
        .check(|value| {
            assert_eq!(16, value.try_usize().unwrap());
        })
        .await;
}

#[fuchsia::test]
async fn divide() {
    Test::test("8 // 2")
        .check(|value| {
            assert_eq!(4, value.try_usize().unwrap());
        })
        .await;
}

#[fuchsia::test]
async fn eq_false() {
    Test::test("8 == 2")
        .check(|value| {
            let Value::Bool(value) = value else {
                panic!();
            };
            assert!(!value);
        })
        .await;
}

#[fuchsia::test]
async fn eq_true() {
    Test::test("2 == 2")
        .check(|value| {
            let Value::Bool(value) = value else {
                panic!();
            };
            assert!(value);
        })
        .await;
}

#[fuchsia::test]
async fn function_decl() {
    Test::test(
        r#"
    def f a b? c.. {
        [$a, $b, $c]
    };

    let a = f 1;
    let b = f 2 3;
    let c = f 4 5 6 7;
    [$a, $b, $c]
    "#,
    )
    .check(|value| {
        let Value::List(mut value) = value else {
            panic!();
        };
        let c = value.pop().unwrap();
        let b = value.pop().unwrap();
        let a = value.pop().unwrap();
        assert!(value.is_empty());

        let Value::List(mut a) = a else {
            panic!();
        };

        assert!(matches!(a.pop().unwrap(), Value::Null));
        assert!(matches!(a.pop().unwrap(), Value::Null));
        assert_eq!(1, a.pop().unwrap().try_usize().unwrap());
        assert!(a.is_empty());

        let Value::List(mut b) = b else {
            panic!();
        };

        assert!(matches!(b.pop().unwrap(), Value::Null));
        assert_eq!(3, b.pop().unwrap().try_usize().unwrap());
        assert_eq!(2, b.pop().unwrap().try_usize().unwrap());
        assert!(b.is_empty());

        let Value::List(mut c) = c else {
            panic!();
        };

        let Value::List(mut c3) = c.pop().unwrap() else {
            panic!();
        };

        assert_eq!(5, c.pop().unwrap().try_usize().unwrap());
        assert_eq!(4, c.pop().unwrap().try_usize().unwrap());
        assert!(c.is_empty());

        assert_eq!(7, c3.pop().unwrap().try_usize().unwrap());
        assert_eq!(6, c3.pop().unwrap().try_usize().unwrap());
        assert!(c3.is_empty());
    })
    .await;
}

#[fuchsia::test]
async fn ge_false() {
    Test::test("2 >= 8")
        .check(|value| {
            let Value::Bool(value) = value else {
                panic!();
            };
            assert!(!value);
        })
        .await;
}

#[fuchsia::test]
async fn ge_true_eq() {
    Test::test("2 >= 2")
        .check(|value| {
            let Value::Bool(value) = value else {
                panic!();
            };
            assert!(value);
        })
        .await;
}

#[fuchsia::test]
async fn ge_true_gt() {
    Test::test("3 >= 2")
        .check(|value| {
            let Value::Bool(value) = value else {
                panic!();
            };
            assert!(value);
        })
        .await;
}

#[fuchsia::test]
async fn gt_false() {
    Test::test("2 > 8")
        .check(|value| {
            let Value::Bool(value) = value else {
                panic!();
            };
            assert!(!value);
        })
        .await;
}

#[fuchsia::test]
async fn gt_true() {
    Test::test("3 > 2")
        .check(|value| {
            let Value::Bool(value) = value else {
                panic!();
            };
            assert!(value);
        })
        .await;
}

#[fuchsia::test]
async fn if_test() {
    Test::test(
        r#"
    if $true {
        3
    } else if $false {
        4
    } else {
        5
    }
    "#,
    )
    .check(|value| {
        assert_eq!(3, value.try_usize().unwrap());
    })
    .await;
}

#[fuchsia::test]
async fn if_test_elif() {
    Test::test(
        r#"
    if $false {
        3
    } else if $true {
        4
    } else {
        5
    }
    "#,
    )
    .check(|value| {
        assert_eq!(4, value.try_usize().unwrap());
    })
    .await;
}

#[fuchsia::test]
async fn if_test_else() {
    Test::test(
        r#"
    if $false {
        3
    } else if $false {
        4
    } else {
        5
    }
    "#,
    )
    .check(|value| {
        assert_eq!(5, value.try_usize().unwrap());
    })
    .await;
}

#[fuchsia::test]
async fn iterate() {
    Test::test("[1, 2, 3] |> $_ * 4")
        .check_async(|value| async move {
            let Value::OutOfLine(PlaygroundValue::Iterator(mut i)) = value else {
                panic!();
            };
            let mut got = Vec::new();
            while let Some(k) = i.next().await.unwrap() {
                got.push(k.try_usize().unwrap());
            }

            assert_eq!(&[4, 8, 12], got.as_slice());
        })
        .await;
}

#[fuchsia::test]
async fn le_false() {
    Test::test("8 <= 2")
        .check(|value| {
            let Value::Bool(value) = value else {
                panic!();
            };
            assert!(!value);
        })
        .await;
}

#[fuchsia::test]
async fn le_true_eq() {
    Test::test("2 <= 2")
        .check(|value| {
            let Value::Bool(value) = value else {
                panic!();
            };
            assert!(value);
        })
        .await;
}

#[fuchsia::test]
async fn le_true_lt() {
    Test::test("2 <= 3")
        .check(|value| {
            let Value::Bool(value) = value else {
                panic!();
            };
            assert!(value);
        })
        .await;
}

#[fuchsia::test]
async fn lt_false() {
    Test::test("8 < 2")
        .check(|value| {
            let Value::Bool(value) = value else {
                panic!();
            };
            assert!(!value);
        })
        .await;
}

#[fuchsia::test]
async fn lt_true() {
    Test::test("2 < 3")
        .check(|value| {
            let Value::Bool(value) = value else {
                panic!();
            };
            assert!(value);
        })
        .await;
}

#[fuchsia::test]
async fn lambda() {
    Test::test(
        r#"
    const f = \(a b? c..) {
        [$a, $b, $c]
    };

    let a = f 1;
    let b = f 2 3;
    let c = f 4 5 6 7;
    [$a, $b, $c]
    "#,
    )
    .check(|value| {
        let Value::List(mut value) = value else {
            panic!();
        };
        let c = value.pop().unwrap();
        let b = value.pop().unwrap();
        let a = value.pop().unwrap();
        assert!(value.is_empty());

        let Value::List(mut a) = a else {
            panic!();
        };

        assert!(matches!(a.pop().unwrap(), Value::Null));
        assert!(matches!(a.pop().unwrap(), Value::Null));
        assert_eq!(1, a.pop().unwrap().try_usize().unwrap());
        assert!(a.is_empty());

        let Value::List(mut b) = b else {
            panic!();
        };

        assert!(matches!(b.pop().unwrap(), Value::Null));
        assert_eq!(3, b.pop().unwrap().try_usize().unwrap());
        assert_eq!(2, b.pop().unwrap().try_usize().unwrap());
        assert!(b.is_empty());

        let Value::List(mut c) = c else {
            panic!();
        };

        let Value::List(mut c3) = c.pop().unwrap() else {
            panic!();
        };

        assert_eq!(5, c.pop().unwrap().try_usize().unwrap());
        assert_eq!(4, c.pop().unwrap().try_usize().unwrap());
        assert!(c.is_empty());

        assert_eq!(7, c3.pop().unwrap().try_usize().unwrap());
        assert_eq!(6, c3.pop().unwrap().try_usize().unwrap());
        assert!(c3.is_empty());
    })
    .await;
}

#[fuchsia::test]
async fn and_short_circuit() {
    Test::test(
        r#"
    let x = 0;
    def y {
        $x = 1;
        $true
    };
    $false && {y};
    $x
    "#,
    )
    .check(|value| {
        assert_eq!(0, value.try_usize().unwrap());
    })
    .await;
}

#[fuchsia::test]
async fn and_short_pass() {
    Test::test(
        r#"
    let x = 0;
    def y {
        $x = 1;
        $true
    };
    $true && {y};
    $x
    "#,
    )
    .check(|value| {
        assert_eq!(1, value.try_usize().unwrap());
    })
    .await;
}

#[fuchsia::test]
async fn or_short_circuit() {
    Test::test(
        r#"
    let x = 0;
    def y {
        $x = 1;
        $true
    };
    $false || {y};
    $x
    "#,
    )
    .check(|value| {
        assert_eq!(1, value.try_usize().unwrap());
    })
    .await;
}

#[fuchsia::test]
async fn or_short_pass() {
    Test::test(
        r#"
    let x = 0;
    def y {
        $x = 1;
        $true
    };
    $true || {y};
    $x
    "#,
    )
    .check(|value| {
        assert_eq!(0, value.try_usize().unwrap());
    })
    .await;
}

#[fuchsia::test]
async fn and_true_false() {
    Test::test("$true && $false")
        .check(|value| {
            let Value::Bool(value) = value else {
                panic!();
            };
            assert!(!value);
        })
        .await;
}

#[fuchsia::test]
async fn and_false_true() {
    Test::test("$false && $true")
        .check(|value| {
            let Value::Bool(value) = value else {
                panic!();
            };
            assert!(!value);
        })
        .await;
}

#[fuchsia::test]
async fn and_false_false() {
    Test::test("$false && $false")
        .check(|value| {
            let Value::Bool(value) = value else {
                panic!();
            };
            assert!(!value);
        })
        .await;
}

#[fuchsia::test]
async fn and_true_true() {
    Test::test("$true && $true")
        .check(|value| {
            let Value::Bool(value) = value else {
                panic!();
            };
            assert!(value);
        })
        .await;
}

#[fuchsia::test]
async fn or_true_false() {
    Test::test("$true || $false")
        .check(|value| {
            let Value::Bool(value) = value else {
                panic!();
            };
            assert!(value);
        })
        .await;
}

#[fuchsia::test]
async fn or_false_true() {
    Test::test("$false || $true")
        .check(|value| {
            let Value::Bool(value) = value else {
                panic!();
            };
            assert!(value);
        })
        .await;
}

#[fuchsia::test]
async fn or_false_false() {
    Test::test("$false || $false")
        .check(|value| {
            let Value::Bool(value) = value else {
                panic!();
            };
            assert!(!value);
        })
        .await;
}

#[fuchsia::test]
async fn or_true_true() {
    Test::test("$true || $true")
        .check(|value| {
            let Value::Bool(value) = value else {
                panic!();
            };
            assert!(value);
        })
        .await;
}

#[fuchsia::test]
async fn not_true() {
    Test::test("!$true")
        .check(|value| {
            let Value::Bool(value) = value else {
                panic!();
            };
            assert!(!value);
        })
        .await;
}

#[fuchsia::test]
async fn not_false() {
    Test::test("!$false")
        .check(|value| {
            let Value::Bool(value) = value else {
                panic!();
            };
            assert!(value);
        })
        .await;
}

#[fuchsia::test]
async fn object_member() {
    Test::test("{foo: 6, bar: 7}.bar")
        .check(|value| {
            assert_eq!(7, value.try_usize().unwrap());
        })
        .await;
}

#[fuchsia::test]
async fn list_index() {
    Test::test("[1, 2, 3, 4][2]")
        .check(|value| {
            assert_eq!(3, value.try_usize().unwrap());
        })
        .await;
}

#[fuchsia::test]
async fn object_member_assign() {
    Test::test("let x = {foo: 6, bar: 7}; $x.bar = 5; $x")
        .check(|value| {
            let Value::Object(value) = value else {
                panic!();
            };
            let mut value: std::collections::HashMap<_, _> = value.into_iter().collect();

            let foo = value.remove("foo").unwrap();
            assert_eq!(6, foo.try_usize().unwrap());
            let bar = value.remove("bar").unwrap();
            assert_eq!(5, bar.try_usize().unwrap());
            assert!(value.is_empty());
        })
        .await;
}

#[fuchsia::test]
async fn list_index_assign() {
    Test::test("let x = [1, 2, 3, 4]; $x[2] = 7; $x")
        .check(|value| {
            let Value::List(mut value) = value else {
                panic!();
            };

            assert_eq!(4, value.pop().unwrap().try_usize().unwrap());
            assert_eq!(7, value.pop().unwrap().try_usize().unwrap());
            assert_eq!(2, value.pop().unwrap().try_usize().unwrap());
            assert_eq!(1, value.pop().unwrap().try_usize().unwrap());
            assert!(value.is_empty());
        })
        .await;
}

#[fuchsia::test]
async fn multiply() {
    Test::test("3 * 2")
        .check(|value| {
            assert_eq!(6, value.try_usize().unwrap());
        })
        .await;
}

#[fuchsia::test]
async fn ne_true() {
    Test::test("8 != 2")
        .check(|value| {
            let Value::Bool(value) = value else {
                panic!();
            };
            assert!(value);
        })
        .await;
}

#[fuchsia::test]
async fn ne_false() {
    Test::test("2 != 2")
        .check(|value| {
            let Value::Bool(value) = value else {
                panic!();
            };
            assert!(!value);
        })
        .await;
}

#[fuchsia::test]
async fn negate() {
    Test::test("-(2 + 2)")
        .check(|value| {
            assert_eq!(
                num::rational::BigRational::from_integer(num::BigInt::from(-4)),
                value.try_big_num().unwrap()
            );
        })
        .await;
}

#[fuchsia::test]
async fn pipe() {
    Test::test("1 + 2 | $_ * 4")
        .check(|value| {
            assert_eq!(12, value.try_usize().unwrap());
        })
        .await;
}

#[fuchsia::test]
async fn range() {
    Test::test("1..=3 |> $_ * 4")
        .check_async(|value| async move {
            let Value::OutOfLine(PlaygroundValue::Iterator(mut i)) = value else {
                panic!();
            };
            let mut got = Vec::new();
            while let Some(k) = i.next().await.unwrap() {
                got.push(k.try_usize().unwrap());
            }

            assert_eq!(&[4, 8, 12], got.as_slice());
        })
        .await;
}

#[fuchsia::test]
async fn range_exclusive() {
    Test::test("1..4 |> $_ * 4")
        .check_async(|value| async move {
            let Value::OutOfLine(PlaygroundValue::Iterator(mut i)) = value else {
                panic!();
            };
            let mut got = Vec::new();
            while let Some(k) = i.next().await.unwrap() {
                got.push(k.try_usize().unwrap());
            }

            assert_eq!(&[4, 8, 12], got.as_slice());
        })
        .await;
}

#[fuchsia::test]
async fn subtract() {
    Test::test("2 - 2")
        .check(|value| {
            assert_eq!(0, value.try_usize().unwrap());
        })
        .await;
}
