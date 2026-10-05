//! A Unix-socket IPC server for external clients (bars, launchers), speaking
//! the same newline-delimited JSON as `shoji_wm/ipc`:
//!
//! ```text
//! client -> server   { "id"?: number, "method": string, "params"?: any }
//! server -> client   { "id": number, "result": any }        (response)
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
        atomic::{AtomicBool, Ordering},
    },
    time::Duration,
};

use serde_json::{Value, json};

use crate::{compositor::COMPOSITOR, runtime};

type Handler = Rc<dyn Fn(&Value) -> Result<Value, String>>;

struct Request {
    client: Arc<Client>,
    id: Option<Value>,
    method: String,
    params: Value,
}

struct Client {
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
                    Some(handler) => match handler(&request.params) {
                        Ok(result) => json!({ "result": result }),
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
