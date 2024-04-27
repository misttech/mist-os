// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use anyhow::anyhow;
use fancy_regex::Regex;
use fidl::endpoints::Proxy;
use fidl_codec::{library as lib, Value as FidlValue};
use fidl_fuchsia_io as fio;
use futures::channel::mpsc::{unbounded as unbounded_channel, UnboundedSender};
use futures::channel::oneshot::{channel as oneshot_channel, Sender as OneshotSender};
use futures::future::BoxFuture;
use futures::{Future, FutureExt, Stream, StreamExt};
use std::collections::HashMap;
use std::fmt::Write;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::OnceLock;
use std::sync::{Arc, Mutex};

use crate::compiler::Visitor;
use crate::error::{Error, Result};
use crate::frame::GlobalVariables;
use crate::parser::{Mutability, ParseResult, TabHint};
use crate::value::{InUseHandle, Invocable, PlaygroundValue, ReplayableIterator, Value, ValueExt};

macro_rules! error {
    ($($data:tt)*) => { Error::from(anyhow!($($data)*)) };
}

/// Maximum number of symlinks we can follow on path lookup before exploding.
pub(crate) const SYMLINK_RECURSION_LIMIT: usize = 40;

/// Interior state of a channel server task. See [InterpreterInner::wait_tx_id].
#[derive(Default)]
struct ChannelServerState {
    /// Hashmap of FIDL transaction IDs => Senders to which replies with those
    /// transaction IDs should be forwarded.
    senders: HashMap<u32, OneshotSender<Result<fidl::MessageBufEtc>>>,
    /// If we get a message out of a channel and don't recognize the transaction
    /// ID, the transaction ID and message are recorded here. Most likely it's
    /// just a timing issue, and the task which is expecting that message will
    /// be along shortly to pick it up.
    orphan_messages: HashMap<u32, Result<fidl::MessageBufEtc>>,
    /// Whether we are currently running a task which reads the channel and
    /// responds to server messages.
    server_running: bool,
}

pub(crate) struct InterpreterInner {
    /// FIDL Codec namespace. Contains the imported FIDL type info from FIDL
    /// JSON for all the FIDL this interpreter knows about.
    lib_namespace: lib::Namespace,
    /// Sending a future on this sender causes it to be polled as part of the
    /// interpreter's run future, making this a sort of mini executor.
    task_sender: UnboundedSender<BoxFuture<'static, ()>>,
    /// State for tasks used to read from channels on behalf of other tasks,
    /// solving a synchronization issue that could come if any task could read
    /// from any channel by itself. See [InterpreterInner::wait_tx_id].
    channel_servers: Mutex<HashMap<u32, ChannelServerState>>,
    /// Pool of FIDL transaction IDs for whoever needs one of those.
    tx_id_pool: AtomicU32,
}

impl InterpreterInner {
    /// Fetch the library namespace.
    pub fn lib_namespace(&self) -> &lib::Namespace {
        &self.lib_namespace
    }

    /// Allocate a new transaction ID for use with FIDL messages. This is just a
    /// guaranteed unique `u32`. It is imbued with no special properties.
    pub fn alloc_tx_id(&self) -> u32 {
        self.tx_id_pool.fetch_add(1, Ordering::Relaxed)
    }

    /// Add a new task to this interpreter.
    pub fn push_task(&self, task: impl Future<Output = ()> + Send + 'static) {
        let _ = self.task_sender.unbounded_send(task.boxed());
    }

    /// Waits for the given handle (presumed to be a channel) to return a
    /// FIDL message with the given transaction ID, then sends the message
    /// through the given sender.
    ///
    /// Our model intrinsically allows multiple ownership of handles. If
    /// multiple tasks send requests on a channel, that's fine, they'll arrive
    /// in whatever order and the other end will sort them out. But if the other
    /// end sends a reply, the individual tasks might end up getting eachothers'
    /// replies if they all just naively read from the channel.
    ///
    /// So instead we have this method, which allows us to ask the interpreter
    /// to start pulling messages out from the channel, forward us the one that
    /// has the transaction ID we expect, and keep any others it may find for
    /// other tasks that may call. The interpreter has at most one of these
    /// "channel server" tasks for each handle being read in this way at any
    /// given time. If two tasks try to read from the same channel, one task
    /// will handle both.
    pub fn wait_tx_id(
        self: Arc<Self>,
        handle: InUseHandle,
        tx_id: u32,
        responder: OneshotSender<Result<fidl::MessageBufEtc>>,
    ) {
        let handle_id = match handle.id() {
            Ok(x) => x,
            Err(e) => {
                let _ = responder.send(Err(e));
                return;
            }
        };

        let start_server = {
            let mut servers = self.channel_servers.lock().unwrap();

            let server = servers.entry(handle_id).or_default();

            if let Some(msg) = server.orphan_messages.remove(&tx_id) {
                let _ = responder.send(msg);
                return;
            }

            server.senders.insert(tx_id, responder);

            !std::mem::replace(&mut server.server_running, true)
        };

        if start_server {
            let weak_self = Arc::downgrade(&self);
            let _ = self.push_task(async move {
                enum FailureMode {
                    Decoding(fidl::Error),
                    Handle(Error),
                }

                let error = loop {
                    let mut buf = fidl::MessageBufEtc::default();
                    match handle.read_channel_etc(&mut buf).await {
                        Ok(()) => {
                            let tx_id = match fidl::encoding::decode_transaction_header(buf.bytes())
                            {
                                Ok((fidl::encoding::TransactionHeader { tx_id, .. }, _)) => tx_id,
                                Err(e) => break FailureMode::Decoding(e),
                            };

                            let buf = std::mem::replace(&mut buf, fidl::MessageBufEtc::default());

                            let Some(this) = weak_self.upgrade() else {
                                return;
                            };

                            let mut servers = this.channel_servers.lock().unwrap();
                            let Some(server) = servers.get_mut(&handle_id) else {
                                return;
                            };

                            if let Some(sender) = server.senders.remove(&tx_id) {
                                let _ = sender.send(Ok(buf));
                            } else {
                                let _ = server.orphan_messages.insert(tx_id, Ok(buf));
                            }

                            if server.senders.is_empty() {
                                server.server_running = false;
                                return;
                            }
                        }
                        Err(status) => break FailureMode::Handle(status),
                    }
                };

                let Some(this) = weak_self.upgrade() else {
                    return;
                };
                let mut servers = this.channel_servers.lock().unwrap();
                let Some(server) = servers.get_mut(&handle_id) else {
                    return;
                };

                for (_, sender) in server.senders.drain() {
                    let _ = sender.send(Err(match &error {
                        FailureMode::Decoding(e) => error!("FIDL decoding error: {e}"),
                        FailureMode::Handle(e) => error!("Channel closed: {e}"),
                    }));
                }

                server.server_running = false;
            });
        }
    }

    /// Assuming the given value contains an invocable, run it with the given
    /// arguments. You can set the value of `$_` with `underscore`.
    pub async fn invoke_value(
        self: Arc<Self>,
        invocable: Value,
        mut args: Vec<Value>,
        underscore: Option<Value>,
    ) -> Result<Value> {
        let in_use_handle = match invocable {
            Value::OutOfLine(PlaygroundValue::Invocable(invocable)) => {
                return invocable.invoke(args, underscore).await;
            }
            x => {
                let Some(in_use_handle) = x.to_in_use_handle() else {
                    return Err(error!("Value cannot be invoked"));
                };
                in_use_handle
            }
        };

        let protocol_name = in_use_handle.get_client_protocol()?;

        let protocol = self
            .lib_namespace()
            .lookup(&protocol_name)
            .map_err(|_| error!("Cannot find definition for protocol {protocol_name}"))?;
        let lib::LookupResult::Protocol(protocol) = protocol else {
            return Err(error!("{protocol_name:?} is not invocable"));
        };

        if args.len() > 1 {
            return Err(error!("Channel message argument must be a single named object"));
        }

        let arg = if let Some(arg) = args.pop() {
            arg
        } else if let Some(underscore) = underscore {
            underscore
        } else {
            return Err(error!("Channel message not provided"));
        };

        let Value::OutOfLine(PlaygroundValue::TypeHinted(method_name, arg)) = arg else {
            return Err(error!("Channel message argument must be a single named object"));
        };

        let Some(method) = protocol.methods.get(&method_name) else {
            return Err(error!("No method {method_name} in {protocol_name}"));
        };

        let request = if let Some(request_type) = &method.request {
            arg.to_fidl_value(self.lib_namespace(), request_type)?
        } else {
            FidlValue::Null
        };

        let tx_id =
            if method.has_response && method.response.is_some() { self.alloc_tx_id() } else { 0 };

        let (bytes, mut handles) = fidl_codec::encode_request(
            self.lib_namespace(),
            tx_id,
            &protocol_name,
            &method.name,
            request,
        )?;
        in_use_handle.write_channel_etc(&bytes, &mut handles)?;

        if tx_id != 0 {
            let (sender, receiver) = oneshot_channel();

            Arc::clone(&self).wait_tx_id(in_use_handle, tx_id, sender);

            let (bytes, handles) = receiver.await??.split();

            let value = fidl_codec::decode_response(self.lib_namespace(), &bytes, handles)?;

            Ok(value.1.upcast())
        } else {
            Ok(Value::Null)
        }
    }

    /// Helper for [`InterpreterInner::open`] that sends a request given a value
    /// of $fs_root and a path.
    fn send_open_request(
        &self,
        fs_root: &InUseHandle,
        path: &String,
        flags: fio::OpenFlags,
    ) -> Result<fio::NodeProxy> {
        let (node, server) = fidl::endpoints::create_proxy::<fio::NodeMarker>()?;

        let request = FidlValue::Object(vec![
            ("flags".to_owned(), FidlValue::U32(flags.bits())),
            ("mode".to_owned(), FidlValue::U32(fio::ModeType::empty().bits())),
            ("path".to_owned(), FidlValue::String(path.clone())),
            (
                "object".to_owned(),
                FidlValue::ServerEnd(server.into_channel(), fio::NODE_PROTOCOL_NAME.to_owned()),
            ),
        ]);

        let (bytes, mut handles) = fidl_codec::encode_request(
            self.lib_namespace(),
            0,
            fio::DIRECTORY_PROTOCOL_NAME,
            "Open",
            request,
        )?;
        fs_root.write_channel_etc(&bytes, &mut handles)?;
        Ok(node)
    }

    /// Open a file in this interpreter's namespace.
    pub async fn open(&self, path: String, fs_root: Value, pwd: Value) -> Result<Value> {
        let Some(fs_root) = fs_root.to_in_use_handle() else {
            return Err(error!("$fs_root is not a handle"));
        };

        let mut path = canonicalize_path(path, pwd)?;

        if !fs_root
            .get_client_protocol()
            .ok()
            .map(|x| self.lib_namespace().inherits(&x, fio::DIRECTORY_PROTOCOL_NAME))
            .unwrap_or(false)
        {
            return Err(error!("$fs_root is not a directory"));
        }

        for _ in 0..SYMLINK_RECURSION_LIMIT {
            let send_open_request =
                |flags: fio::OpenFlags| self.send_open_request(&fs_root, &path, flags);

            let node = send_open_request(fio::OpenFlags::NODE_REFERENCE)?;
            let (_inode, attr) = node.get_attr().await?;

            let (proto, node) = if attr.mode & fio::MODE_TYPE_MASK == fio::MODE_TYPE_DIRECTORY {
                (
                    fio::DIRECTORY_PROTOCOL_NAME.to_owned(),
                    send_open_request(fio::OpenFlags::RIGHT_READABLE)?,
                )
            } else if attr.mode & fio::MODE_TYPE_MASK == fio::MODE_TYPE_SYMLINK {
                let node = send_open_request(fio::OpenFlags::RIGHT_READABLE)?;
                let symlink = fio::SymlinkProxy::from_channel(
                    node.into_channel().expect("Node proxy somehow cloned!"),
                );
                let symlink_info = symlink.describe().await?;
                let Some(target) = symlink_info.target else {
                    return Err(error!("Symlink at {path} did not contain a target"));
                };
                let target = String::from_utf8(target.clone())
                    .map_err(|_| error!("Followed symlink to non-utf8 path"))?;
                let end = path.rfind('/').expect("Canonicalized path wasn't absolute!");
                let prefix = &path[..end];
                let prefix = if prefix.is_empty() { "/" } else { prefix };
                // Set the path to the new target, reset the flags, and
                // basically start the whole circus over.
                path = canonicalize_path_dont_check_pwd(target, prefix);
                continue;
            } else if (attr.mode & fio::MODE_TYPE_MASK == fio::MODE_TYPE_BLOCK_DEVICE)
                || (attr.mode & fio::MODE_TYPE_MASK == fio::MODE_TYPE_FILE)
            {
                (
                    fio::FILE_PROTOCOL_NAME.to_owned(),
                    send_open_request(fio::OpenFlags::RIGHT_READABLE)?,
                )
            } else if attr.mode & fio::MODE_TYPE_MASK == fio::MODE_TYPE_SERVICE {
                let node = send_open_request(fio::OpenFlags::empty())?;
                let end = path.rfind('/').expect("Canonicalized path wasn't absolute!");

                let name = &path[end + 1..];
                if name.starts_with("fuchsia.") {
                    let mut ret = name.to_owned();
                    let dot = ret.rfind('.').unwrap();
                    ret.replace_range(dot..dot + 1, "/");
                    (ret, node)
                } else {
                    (fio::NODE_PROTOCOL_NAME.to_owned(), node)
                }
            } else {
                return Err(error!("Unknown attributes {attr:?}"));
            };

            return Ok(Value::ClientEnd(
                node.into_channel().expect("Could not tear down proxy").into_zx_channel(),
                proto,
            ));
        }

        Err(error!("Symlink recursion depth exceeded attempting to open {path}"))
    }
}

pub struct Interpreter {
    pub(crate) inner: Arc<InterpreterInner>,
    global_variables: Mutex<GlobalVariables>,
}

impl Interpreter {
    /// Create a new interpreter.
    ///
    /// The interpreter itself may spawn multiple tasks, and polling the future
    /// returned alongside the interpreter is necessary to keep those tasks
    /// running, and thus the interpreter functioning correctly.
    pub async fn new(
        lib_namespace: lib::Namespace,
        fs_root: fidl::endpoints::ClientEnd<fidl_fuchsia_io::DirectoryMarker>,
    ) -> (Self, impl Future<Output = ()>) {
        let (task_sender, task_receiver) = unbounded_channel();

        let fs_root = Value::ClientEnd(fs_root.into_channel(), "fuchsia.io/Directory".to_owned());
        let mut global_variables = GlobalVariables::default();
        global_variables.define("fs_root".to_owned(), Ok(fs_root), Mutability::Mutable);
        global_variables.define(
            "pwd".to_owned(),
            Ok(Value::String("/".to_owned())),
            Mutability::Mutable,
        );
        let interpreter = Interpreter {
            inner: Arc::new(InterpreterInner {
                lib_namespace,
                task_sender,
                channel_servers: Mutex::new(HashMap::new()),
                tx_id_pool: AtomicU32::new(1),
            }),
            global_variables: Mutex::new(global_variables),
        };
        let mut executor = task_receiver.for_each_concurrent(None, |x| x);
        interpreter.add_builtins(&mut executor).await;

        (interpreter, executor)
    }

    /// Take a [`ReplayableIterator`], which is how the playground [`Value`]
    /// type represents iterators, and convert it to a [`futures::Stream`],
    /// which is easier to work with directly in Rust.
    pub fn replayable_iterator_to_stream(
        &self,
        mut iter: ReplayableIterator,
    ) -> impl Stream<Item = Result<Value>> {
        let (sender, receiver) = unbounded_channel();

        self.inner.push_task(async move {
            loop {
                match iter.next().await {
                    Ok(Some(v)) => {
                        if sender.unbounded_send(Ok(v)).is_err() {
                            break;
                        }
                    }
                    Ok(None) => break,
                    Err(e) => {
                        let _ = sender.unbounded_send(Err(e));
                        break;
                    }
                }
            }
        });

        receiver
    }

    /// Compile the given code as an anonymous callable value in this
    /// interpreter's scope, then return it as a Rust closure so you can call
    /// the code repeatedly.
    pub async fn get_runnable(
        &self,
        code: &str,
    ) -> Result<impl (Fn() -> BoxFuture<'static, Result<Value>>) + Clone> {
        let Value::OutOfLine(PlaygroundValue::Invocable(program)) =
            self.run(format!("\\() {{ {code} }}").as_str()).await?
        else {
            unreachable!("Preamble didn't compile to invocable");
        };

        Ok(move || program.clone().invoke(vec![], None).boxed())
    }

    /// Add a new command to the global environment.
    ///
    /// Creates a global variable with the given `name` in this interpreter's
    /// global scope, and assigns it an invocable value which calls the closure
    /// given in `cmd`. The closure takes a list of arguments as [`Value`]s and
    /// an optional `Value` which is the current value of `$_`
    pub async fn add_command<
        F: Fn(Vec<Value>, Option<Value>) -> R + Send + Sync + 'static,
        R: Future<Output = Result<Value>> + Send + 'static,
    >(
        &self,
        name: &str,
        cmd: F,
    ) {
        let cmd = Arc::new(cmd);
        let cmd = move |args: Vec<Value>, underscore: Option<Value>| {
            let cmd = Arc::clone(&cmd);

            cmd(args.into(), underscore).boxed()
        };

        self.global_variables.lock().unwrap().define(
            name.to_owned(),
            Ok(Value::OutOfLine(PlaygroundValue::Invocable(Invocable::new(Arc::new(cmd))))),
            Mutability::Constant,
        );
    }

    /// Parse and run the given program in the context of this interpreter.
    pub async fn run<'a, T: std::convert::Into<ParseResult<'a>>>(
        &self,
        program: T,
    ) -> Result<Value> {
        let program: ParseResult<'a> = program.into();

        if !program.errors.is_empty() {
            let mut s = String::new();
            for error in program.errors.into_iter() {
                writeln!(
                    s,
                    "line {} column {}: {}",
                    error.0.location_line(),
                    error.0.get_utf8_column(),
                    error.1
                )?;
            }

            Err(error!(s))
        } else {
            let mut visitor = Visitor::new();
            let compiled = visitor.visit(program.tree);
            let (mut frame, invalid_ids) = {
                let mut global_variables = self.global_variables.lock().unwrap();
                // TODO: There's a complicated bug here where only the last
                // declaration of a variable determines the mutability for the
                // whole run of this compilation unit. We could fix it by not
                // reusing slots for multiple declarations.
                for (name, mutability) in visitor.get_top_level_variable_decls() {
                    global_variables.ensure_defined(
                        name,
                        || Err(error!("'{name}' undeclared")),
                        mutability,
                    )
                }
                let slots_needed = visitor.slots_needed();
                let (mut captured_ids, allocated_ids) = visitor.into_slot_data();
                (
                    global_variables.as_frame(slots_needed, |ident| {
                        if let Some(id) = captured_ids.remove(ident) {
                            Some(id)
                        } else {
                            allocated_ids.get(ident).copied()
                        }
                    }),
                    captured_ids,
                )
            };

            for (name, slot) in invalid_ids {
                frame.assign(slot, Err(error!("'{name}' undeclared")));
            }

            let frame = Mutex::new(frame);
            compiled(&self.inner, &frame).await
        }
    }

    /// Performs tab completion. Takes in a command string and the cursor
    /// position and returns a list of tuples of strings to be inserted and
    /// where in the string they begin. The characters between the cursor
    /// position and the beginning position given with the completion should be
    /// deleted before the completion text is inserted in their place, and the
    /// cursor should end up at the end of the completion text.
    pub async fn complete(&self, cmd: String, cursor_pos: usize) -> Vec<(String, usize)> {
        let cmd: ParseResult<'_> = cmd.as_str().into();
        let whitespace = cmd.whitespace;
        let hints = cmd.tab_completions;
        let mut ret = Vec::new();

        let mut whitespace_range_start = cursor_pos;
        let mut whitespace_range_end = cursor_pos;

        for whitespace in whitespace.iter() {
            let whitespace_start = whitespace.location_offset();
            let whitespace_end = whitespace_start + whitespace.fragment().len();
            if whitespace_start <= cursor_pos && whitespace_end >= cursor_pos {
                whitespace_range_start = std::cmp::min(whitespace_range_start, whitespace_start);
                whitespace_range_end = std::cmp::max(whitespace_range_end, whitespace_end);
            }
        }

        for hint in hints.values().flat_map(|x| x.iter()) {
            let start = hint.span().location_offset();
            let end = start + hint.span().fragment().len();
            if start > whitespace_range_end {
                continue;
            }

            if end < whitespace_range_start {
                continue;
            }

            let fragment = if hint.span().fragment().is_empty() {
                ""
            } else {
                if start > cursor_pos || end < cursor_pos {
                    continue;
                }

                &hint.span().fragment()[..cursor_pos - start]
            };

            match hint {
                TabHint::Invocable(_) => {
                    for name in self.global_variables.lock().unwrap().names(|x| x.is_invocable()) {
                        if name.starts_with(fragment) {
                            ret.push((format!("{name} "), start))
                        }
                    }
                }
                TabHint::CommandArgument(_) => {
                    if fragment.is_empty() {
                        continue;
                    }

                    let file = self.open(fragment.to_owned()).await;

                    let (file, prefix, filter) = if let Ok(file) = file {
                        (file, fragment, "")
                    } else if fragment.ends_with("/") {
                        continue;
                    } else {
                        let (prefix, filter) = fragment.rsplit_once("/").unwrap_or((".", fragment));
                        let Ok(file) = self.open(prefix.to_owned()).await else {
                            continue;
                        };
                        (file, prefix, filter)
                    };

                    let dir =
                        file.try_client_channel(self.inner.lib_namespace(), "fuchsia.io/Directory");

                    if let Ok(dir) = dir {
                        let dir = fio::DirectoryProxy::from_channel(
                            fuchsia_async::Channel::from_channel(dir),
                        );
                        let Ok(entries) = fuchsia_fs::directory::readdir(&dir).await else {
                            continue;
                        };

                        for entry in entries.into_iter().filter(|x| x.name.starts_with(filter)) {
                            let (node, server) = fidl::endpoints::create_proxy().unwrap();
                            let is_dir = if let Ok(_) = dir.open(
                                fio::OpenFlags::NODE_REFERENCE,
                                fio::ModeType::empty(),
                                &entry.name,
                                server,
                            ) {
                                node.get_attr()
                                    .await
                                    .map(|attr| {
                                        attr.1.mode & fio::MODE_TYPE_MASK
                                            == fio::MODE_TYPE_DIRECTORY
                                    })
                                    .unwrap_or(false)
                            } else {
                                false
                            };

                            let postfix = if is_dir { "/" } else { " " };
                            let sep = if prefix.ends_with("/") { "" } else { "/" };
                            let name = entry.name;
                            ret.push((format!("{prefix}{sep}{name}{postfix}"), start));
                        }
                    } else if prefix == fragment && !fragment.ends_with("/") {
                        ret.push((format!("{fragment} "), start));
                    }
                }
            }
        }

        ret
    }

    /// Open a file in this interpreter's namespace.
    pub async fn open(&self, path: String) -> Result<Value> {
        let (fs_root, pwd) = {
            let globals = self.global_variables.lock().unwrap();
            (globals.get("fs_root"), globals.get("pwd"))
        };
        let fs_root = fs_root.await.ok_or_else(|| error!("$fs_root undefined"))??;
        let pwd = pwd.await.ok_or_else(|| error!("$pwd undefined"))??;

        self.inner.open(path, fs_root, pwd).await
    }
}

/// Turn a path into a dotless, absolute form.
pub fn canonicalize_path(path: String, pwd: Value) -> Result<String> {
    let Value::String(pwd) = pwd else {
        return Err(error!("$pwd is not a string"));
    };

    if !pwd.starts_with("/") {
        return Err(error!("$pwd is not an absolute path"));
    }

    Ok(canonicalize_path_dont_check_pwd(path, &pwd))
}

/// Turn a path into a dotless, absolute form. Don't validate the pwd string first.
pub fn canonicalize_path_dont_check_pwd(path: String, pwd: &str) -> String {
    static MULTIPLE_SLASHES: OnceLock<Regex> = OnceLock::new();
    static SINGLE_DOT: OnceLock<Regex> = OnceLock::new();
    static DOUBLE_DOT_ELIMINATION: OnceLock<Regex> = OnceLock::new();

    let multiple_slashes = MULTIPLE_SLASHES.get_or_init(|| Regex::new(r"/{2,}").unwrap());
    let single_dot = SINGLE_DOT.get_or_init(|| Regex::new(r"/\.(?=/|$)").unwrap());
    let double_dot_elimination =
        DOUBLE_DOT_ELIMINATION.get_or_init(|| Regex::new(r"(^|[^/]+)/\.\.(/|$)").unwrap());

    let sep = if pwd.ends_with("/") { "" } else { "/" };
    let path = if path.starts_with("/") { path } else { format!("{pwd}{sep}{path}") };
    let path = multiple_slashes.replace_all(&path, "/");
    let mut path = single_dot.replace_all(&path, "").to_string();
    let mut path_len = path.len();
    loop {
        let new_path = double_dot_elimination.replace(&path, "");
        if new_path.len() == path_len {
            break;
        }
        path = new_path.to_string();
        path_len = path.len();
    }

    if path.ends_with("/") {
        path.pop();
    }

    if path.is_empty() {
        path.push('/');
    }

    path.to_string()
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn canonicalize() {
        assert_eq!(
            "/foo",
            &canonicalize_path(
                "././/baz/bang/../../..".to_owned(),
                Value::String("/foo/bar".to_owned())
            )
            .unwrap()
        );
    }
}
