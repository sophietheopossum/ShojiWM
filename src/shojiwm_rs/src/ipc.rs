//! A Unix-socket IPC server for external clients (bars, launchers), speaking
//! the same newline-delimited JSON as `shoji_wm/ipc`:
//!
//! ```text
//! client -> server   { "id"?: number, "method": string, "params"?: any }
//! server -> client   { "id": number, "result": any }        (response)
//!                    { "id": number }                       (response, no result)
//!                    { "id": number, "error": string }      (error)
//!                    { "event": string, "payload": any }    (broadcast)
//! ```
//!
//! Sockets are served on background threads; handlers run on the compositor
//! thread like every other config callback.
//!
//! ```no_run
//! use shojiwm_rs::{ipc::IpcServer, prelude::*};
//!
//! let ipc = IpcServer::new().expect("socket");
//! ipc.handle("ping", |_params| Ok(serde_json::json!("pong")));
//! // Push to the caller only, e.g. for a subscription.
//! ipc.handle_with_client("subscribe", |_params, client| {
//!     client.send("subscribed", serde_json::json!({ "client": client.id() }));
//!     Ok(None)
//! });
//! ipc.broadcast("hello", serde_json::json!({ "from": "config" }));
//! ```

use std::{
    cell::RefCell,
    collections::HashMap,
    io::{BufRead, BufReader, ErrorKind, Write},
    os::unix::net::{UnixListener, UnixStream},
    os::unix::fs::FileTypeExt,
    path::{Path, PathBuf},
    rc::Rc,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    time::Duration,
};

use serde_json::{Value, json};

use crate::{compositor::COMPOSITOR, runtime};

static NEXT_CLIENT_ID: AtomicU64 = AtomicU64::new(1);

type Handler = Rc<dyn Fn(&Value, &IpcClient) -> Result<Option<Value>, String>>;

struct Request {
    client: Arc<Client>,
    id: Option<Value>,
    method: String,
    params: Value,
}

struct Client {
    id: u64,
    stream: Mutex<UnixStream>,
    alive: AtomicBool,
}

impl Client {
    fn write(&self, message: &Value) {
        if !self.alive.load(Ordering::Relaxed) {
            return;
        }
        let mut frame = message.to_string();
        frame.push('\n');
        let ok = self
            .stream
            .lock()
            .map(|mut stream| stream.write_all(frame.as_bytes()).is_ok())
            .unwrap_or(false);
        if !ok {
            self.alive.store(false, Ordering::Relaxed);
        }
    }
}

/// The connection a request came from: answer it beyond the reply, e.g. push
/// events to only the clients that asked for them.
#[derive(Clone)]
pub struct IpcClient(Arc<Client>);

impl IpcClient {
    /// Push `{ event, payload }` to this client only.
    pub fn send(&self, event: &str, payload: Value) {
        self.0.write(&json!({ "event": event, "payload": payload }));
    }

    /// Whether the connection is still open.
    pub fn is_alive(&self) -> bool {
        self.0.alive.load(Ordering::Relaxed)
    }

    /// Unique per connection, for keying per-client state.
    pub fn id(&self) -> u64 {
        self.0.id
    }
}

/// The socket path `shoji_wm/ipc` uses: `$XDG_RUNTIME_DIR/shojiwm-$WAYLAND_DISPLAY.sock`.
pub fn default_socket_path() -> PathBuf {
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR").unwrap_or_else(|_| "/tmp".into());
    let display = std::env::var("WAYLAND_DISPLAY").unwrap_or_else(|_| "wayland-0".into());
    PathBuf::from(runtime_dir).join(format!("shojiwm-{display}.sock"))
}

/// A running IPC server. Clone it freely; it stops when [`close`](Self::close)d.
#[derive(Clone)]
pub struct IpcServer {
    handlers: Rc<RefCell<HashMap<String, Handler>>>,
    clients: Arc<Mutex<Vec<Arc<Client>>>>,
    closed: Arc<AtomicBool>,
    path: Rc<RefCell<Option<PathBuf>>>,
}

impl IpcServer {
    /// Serve on [`default_socket_path`]. The path names the compositor's
    /// `WAYLAND_DISPLAY`, which is only known once the config is enabled, so
    /// a server created while the config is set up starts listening then;
    /// handlers can be registered right away either way. A failure to listen
    /// at that point (see [`bind`](Self::bind)) can only be logged, so this
    /// returns an error only when the config is already enabled.
    pub fn new() -> std::io::Result<Self> {
        Self::deferred(default_socket_path)
    }

    fn deferred(path: impl Fn() -> PathBuf + 'static) -> std::io::Result<Self> {
        let server = Self::unbound();
        if runtime::is_enabled() {
            server.listen(path())?;
        } else {
            let deferred = server.clone();
            COMPOSITOR.on_enable(move |_| {
                if let Err(error) = deferred.listen(path()) {
                    tracing::warn!(%error, "config IPC socket unavailable");
                }
            });
        }
        Ok(server)
    }

    /// Serve on `path` right away. A socket file nothing answers on is
    /// replaced; a path another server still answers on fails with
    /// `AddrInUse`, and one that is not a socket is never removed.
    pub fn bind(path: PathBuf) -> std::io::Result<Self> {
        let server = Self::unbound();
        server.listen(path)?;
        Ok(server)
    }

    fn unbound() -> Self {
        Self {
            handlers: Rc::default(),
            clients: Arc::default(),
            closed: Arc::new(AtomicBool::new(false)),
            path: Rc::default(),
        }
    }

    fn listen(&self, path: PathBuf) -> std::io::Result<()> {
        if self.closed.load(Ordering::Relaxed) {
            return Ok(());
        }
        remove_stale_socket(&path)?;
        let listener = UnixListener::bind(&path)?;
        *self.path.borrow_mut() = Some(path);

        let sender = {
            let handlers = self.handlers.clone();
            COMPOSITOR.channel(move |request: Request| {
                let handler = handlers.borrow().get(&request.method).cloned();
                let response = match handler {
                    Some(handler) => match handler(&request.params, &IpcClient(request.client.clone())) {
                        Ok(Some(result)) => json!({ "result": result }),
                        // Like a TypeScript handler that returns nothing.
                        Ok(None) => json!({}),
                        Err(error) => json!({ "error": error }),
                    },
                    None => json!({ "error": format!("unknown method: {}", request.method) }),
                };
                if let Some(id) = request.id {
                    let mut response = response;
                    response["id"] = id;
                    request.client.write(&response);
                }
            })
        };

        let clients = self.clients.clone();
        let closed = self.closed.clone();
        std::thread::Builder::new()
            .name("shoji-ipc-accept".into())
            .spawn(move || {
                for stream in listener.incoming() {
                    if closed.load(Ordering::Relaxed) {
                        break;
                    }
                    let Ok(stream) = stream else {
                        continue;
                    };
                    let _ = stream.set_write_timeout(Some(Duration::from_millis(200)));
                    let Ok(reader) = stream.try_clone() else {
                        continue;
                    };
                    let client = Arc::new(Client {
                        id: NEXT_CLIENT_ID.fetch_add(1, Ordering::Relaxed),
                        stream: Mutex::new(stream),
                        alive: AtomicBool::new(true),
                    });
                    if let Ok(mut clients) = clients.lock() {
                        clients.push(client.clone());
                    }
                    let sender = sender.clone();
                    let _ = std::thread::Builder::new()
                        .name("shoji-ipc-client".into())
                        .spawn(move || {
                            for line in BufReader::new(reader).lines() {
                                let Ok(line) = line else {
                                    break;
                                };
                                let Ok(message) = serde_json::from_str::<Value>(line.trim()) else {
                                    continue;
                                };
                                let Some(method) = message.get("method").and_then(Value::as_str) else {
                                    continue;
                                };
                                sender.send(Request {
                                    client: client.clone(),
                                    id: message.get("id").filter(|id| !id.is_null()).cloned(),
                                    method: method.to_owned(),
                                    params: message.get("params").cloned().unwrap_or(Value::Null),
                                });
                            }
                            client.alive.store(false, Ordering::Relaxed);
                        });
                }
            })?;
        Ok(())
    }

    /// Answer `method`; the result (or error) goes back to the caller when
    /// it sent an `id`.
    pub fn handle(&self, method: &str, handler: impl Fn(&Value) -> Result<Value, String> + 'static) {
        self.handle_with_client(method, move |params, _client| handler(params).map(Some));
    }

    /// Like [`handle`](Self::handle), but the handler also gets the calling
    /// client. Returning `Ok(None)` replies `{ "id" }` without a `result`,
    /// as a TypeScript handler that returns nothing does.
    pub fn handle_with_client(
        &self,
        method: &str,
        handler: impl Fn(&Value, &IpcClient) -> Result<Option<Value>, String> + 'static,
    ) {
        self.handlers
            .borrow_mut()
            .insert(method.to_owned(), Rc::new(handler));
    }

    /// Push `{ event, payload }` to every connected client.
    pub fn broadcast(&self, event: &str, payload: Value) {
        let message = json!({ "event": event, "payload": payload });
        let clients: Vec<Arc<Client>> = match self.clients.lock() {
            Ok(mut clients) => {
                clients.retain(|client| client.alive.load(Ordering::Relaxed));
                clients.clone()
            }
            Err(_) => return,
        };
        for client in clients {
            client.write(&message);
        }
    }

    pub fn client_count(&self) -> usize {
        self.clients
            .lock()
            .map(|clients| clients.iter().filter(|client| client.alive.load(Ordering::Relaxed)).count())
            .unwrap_or(0)
    }

    pub fn close(&self) {
        self.closed.store(true, Ordering::Relaxed);
        if let Some(path) = self.path.borrow_mut().take() {
            // Unblock the accept loop.
            let _ = UnixStream::connect(&path);
            let _ = std::fs::remove_file(&path);
        }
        if let Ok(mut clients) = self.clients.lock() {
            for client in clients.drain(..) {
                client.alive.store(false, Ordering::Relaxed);
                if let Ok(stream) = client.stream.lock() {
                    let _ = stream.shutdown(std::net::Shutdown::Both);
                }
            }
        }
    }
}

/// Clear a socket file left behind by a server that is gone, but never take
/// over one that still answers: that is another compositor's live socket,
/// e.g. the session's own while this one runs nested inside it.
fn remove_stale_socket(path: &Path) -> std::io::Result<()> {
    match UnixStream::connect(path) {
        Ok(_) => Err(std::io::Error::new(
            ErrorKind::AddrInUse,
            format!("{} is served by another process", path.display()),
        )),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) if error.kind() == ErrorKind::ConnectionRefused => {
            // connect() is also refused on a regular file; leave those alone.
            if !std::fs::symlink_metadata(path)?.file_type().is_socket() {
                return Err(std::io::Error::new(
                    ErrorKind::AlreadyExists,
                    format!("{} exists and is not a socket", path.display()),
                ));
            }
            std::fs::remove_file(path)
        }
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    /// A directory for one test's sockets, removed when the test ends.
    struct Scratch(PathBuf);

    impl Scratch {
        fn new() -> Self {
            static NEXT: AtomicUsize = AtomicUsize::new(0);
            let dir = std::env::temp_dir().join(format!(
                "shojiwm-rs-ipc-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            std::fs::create_dir_all(&dir).expect("scratch dir");
            Self(dir)
        }

        fn path(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn bind_leaves_a_live_socket_alone() {
        let scratch = Scratch::new();
        let path = scratch.path("live.sock");
        let live = UnixListener::bind(&path).expect("live server");

        let error = IpcServer::bind(path.clone()).err().expect("a served path must be refused");
        assert_eq!(error.kind(), ErrorKind::AddrInUse);

        // The live server keeps its socket and still accepts.
        assert!(path.exists());
        let _client = UnixStream::connect(&path).expect("live socket still answers");
        assert!(live.accept().is_ok());
    }

    #[test]
    fn bind_replaces_a_stale_socket() {
        let scratch = Scratch::new();
        let path = scratch.path("stale.sock");
        drop(UnixListener::bind(&path).expect("old server"));
        assert!(path.exists(), "a dropped listener leaves its file behind");

        let server = IpcServer::bind(path.clone()).expect("a stale socket is taken over");
        assert!(UnixStream::connect(&path).is_ok());
        server.close();
        assert!(!path.exists());
    }

    /// Run the queued requests as a runtime turn would, until `done`.
    fn serve_until(mut done: impl FnMut() -> bool) {
        for _ in 0..400 {
            let channels = runtime::with_registry(|registry| registry.channels.clone());
            for channel in channels {
                channel();
            }
            if done() {
                return;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        panic!("IPC request was not served");
    }

    fn connect(path: &Path) -> (UnixStream, BufReader<UnixStream>) {
        let stream = UnixStream::connect(path).expect("connect");
        stream.set_read_timeout(Some(Duration::from_millis(5))).expect("timeout");
        let reader = BufReader::new(stream.try_clone().expect("clone"));
        (stream, reader)
    }

    fn try_read(reader: &mut BufReader<UnixStream>) -> Option<Value> {
        let mut line = String::new();
        match reader.read_line(&mut line) {
            Ok(n) if n > 0 => Some(serde_json::from_str(line.trim()).expect("frame is JSON")),
            _ => None,
        }
    }

    #[test]
    fn handlers_can_answer_their_caller_only() {
        runtime::reset();
        let scratch = Scratch::new();
        let path = scratch.path("clients.sock");
        let server = IpcServer::bind(path.clone()).expect("server");
        server.handle_with_client("subscribe", |_params, client| {
            client.send("subscribed", json!({ "client": client.id() }));
            Ok(Some(json!(client.id())))
        });
        server.handle_with_client("quiet", |_params, _client| Ok(None));

        let (mut caller, mut caller_in) = connect(&path);
        let (_bystander, mut bystander_in) = connect(&path);
        writeln!(caller, r#"{{"id":1,"method":"subscribe"}}"#).expect("send");

        let mut frames = Vec::new();
        serve_until(|| {
            while let Some(frame) = try_read(&mut caller_in) {
                frames.push(frame);
            }
            frames.len() == 2
        });
        let id = frames[1]["result"].as_u64().expect("the reply carries the client id");
        assert_eq!(frames[0], json!({ "event": "subscribed", "payload": { "client": id } }));
        assert!(try_read(&mut bystander_in).is_none(), "the other client gets nothing");

        writeln!(caller, r#"{{"id":2,"method":"quiet"}}"#).expect("send");
        let mut reply = None;
        serve_until(|| {
            reply = reply.take().or_else(|| try_read(&mut caller_in));
            reply.is_some()
        });
        assert_eq!(reply, Some(json!({ "id": 2 })), "no result key, like a void TS handler");
        server.close();
        runtime::reset();
    }

    #[test]
    fn bind_never_removes_a_file_that_is_not_a_socket() {
        let scratch = Scratch::new();
        let path = scratch.path("file.sock");
        std::fs::write(&path, "not a socket").expect("file");

        let error = IpcServer::bind(path.clone()).err().expect("a regular file must be refused");
        assert_eq!(error.kind(), ErrorKind::AlreadyExists);
        assert_eq!(std::fs::read_to_string(&path).expect("file kept"), "not a socket");
    }

    #[test]
    fn a_server_made_during_setup_listens_once_enabled() {
        runtime::reset();
        let scratch = Scratch::new();
        let path = scratch.path("deferred.sock");
        let server = {
            let path = path.clone();
            IpcServer::deferred(move || path.clone()).expect("server")
        };
        assert!(!path.exists(), "nothing is bound during setup");

        runtime::set_enabled(true);
        runtime::emit(
            |listeners| listeners.enable.clone(),
            |listener| listener(&crate::compositor::EnableEvent { reason: "initial".into() }),
        );
        assert!(UnixStream::connect(&path).is_ok(), "listening after enable");
        server.close();
        runtime::reset();
    }
}
