//! Test helpers: a scratch directory, a fake `/proc`, and a fake Discord.

use std::fs;
use std::io::Write;
use std::net::Shutdown;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use serde_json::{json, Value};

use crate::catalog::Fetcher;
use crate::discord::{encode_frame, read_frame, OP_CLOSE, OP_FRAME, OP_HANDSHAKE};

/// A unique directory under the system temp dir, removed on drop.
pub struct TempDir(PathBuf);

impl TempDir {
    pub fn new(tag: &str) -> TempDir {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let name = format!(
            "proton-presence-{tag}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        );
        let path = std::env::temp_dir().join(name);
        fs::create_dir_all(&path).unwrap();
        TempDir(path)
    }

    pub fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// Writes `<proc_root>/<pid>/{comm,cmdline,environ}` like the kernel does.
pub fn add_proc(proc_root: &Path, pid: u32, comm: &str, cmdline: &[&str], env: &[(&str, &str)]) {
    let dir = proc_root.join(pid.to_string());
    fs::create_dir_all(&dir).unwrap();
    fs::write(dir.join("comm"), format!("{comm}\n")).unwrap();
    let cmdline: String = cmdline.iter().map(|arg| format!("{arg}\0")).collect();
    fs::write(dir.join("cmdline"), cmdline).unwrap();
    let environ: String = env.iter().map(|(k, v)| format!("{k}={v}\0")).collect();
    fs::write(dir.join("environ"), environ).unwrap();
}

/// A Discord IPC server on a Unix socket. It answers handshakes and hands
/// every frame it receives to the test.
pub struct FakeDiscord {
    path: PathBuf,
    frames: Receiver<(u32, Value)>,
    streams: Arc<Mutex<Vec<UnixStream>>>,
    stopping: Arc<AtomicBool>,
    acceptor: Option<JoinHandle<()>>,
}

impl FakeDiscord {
    /// Accepts every handshake with a READY event.
    pub fn start(socket: &Path) -> FakeDiscord {
        FakeDiscord::spawn(socket, false)
    }

    /// Answers every handshake with CLOSE, like Discord does for an
    /// unknown client ID.
    pub fn start_rejecting(socket: &Path) -> FakeDiscord {
        FakeDiscord::spawn(socket, true)
    }

    fn spawn(socket: &Path, reject: bool) -> FakeDiscord {
        let listener = UnixListener::bind(socket).unwrap();
        let (tx, frames) = mpsc::channel();
        let streams = Arc::new(Mutex::new(Vec::new()));
        let stopping = Arc::new(AtomicBool::new(false));

        let acceptor = {
            let streams = Arc::clone(&streams);
            let stopping = Arc::clone(&stopping);
            thread::spawn(move || {
                for stream in listener.incoming() {
                    if stopping.load(Ordering::SeqCst) {
                        break;
                    }
                    let Ok(stream) = stream else { break };
                    streams.lock().unwrap().push(stream.try_clone().unwrap());
                    let tx = tx.clone();
                    thread::spawn(move || serve(stream, tx, reject));
                }
            })
        };

        FakeDiscord {
            path: socket.to_path_buf(),
            frames,
            streams,
            stopping,
            acceptor: Some(acceptor),
        }
    }

    /// The next frame a client sent. Panics if none arrives.
    pub fn recv(&self) -> (u32, Value) {
        self.try_recv(Duration::from_secs(5))
            .expect("no frame from the client within 5s")
    }

    pub fn try_recv(&self, timeout: Duration) -> Option<(u32, Value)> {
        self.frames.recv_timeout(timeout).ok()
    }

    /// Sends a frame to every connected client.
    pub fn send(&self, opcode: u32, body: &Value) {
        let frame = encode_frame(opcode, body.to_string().as_bytes());
        for stream in self.streams.lock().unwrap().iter_mut() {
            let _ = stream.write_all(&frame);
        }
    }

    /// Quits: drops every connection and removes the socket.
    pub fn stop(self) {}

    fn shutdown(&mut self) {
        if self.stopping.swap(true, Ordering::SeqCst) {
            return;
        }
        // Wake the acceptor so it sees the flag and drops the listener.
        let _ = UnixStream::connect(&self.path);
        if let Some(acceptor) = self.acceptor.take() {
            let _ = acceptor.join();
        }
        for stream in self.streams.lock().unwrap().drain(..) {
            let _ = stream.shutdown(Shutdown::Both);
        }
        let _ = fs::remove_file(&self.path);
    }
}

impl Drop for FakeDiscord {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn serve(mut stream: UnixStream, tx: Sender<(u32, Value)>, reject: bool) {
    let mut buf = Vec::new();
    while let Ok((opcode, payload)) = read_frame(&mut stream, &mut buf) {
        let body: Value = serde_json::from_slice(&payload).unwrap_or(Value::Null);
        let reply = match opcode {
            OP_HANDSHAKE if reject => Some((
                OP_CLOSE,
                json!({ "code": 4000, "message": "Invalid Client ID" }),
            )),
            OP_HANDSHAKE => Some((
                OP_FRAME,
                json!({ "cmd": "DISPATCH", "evt": "READY", "data": { "v": 1 }, "nonce": null }),
            )),
            _ => None,
        };
        if tx.send((opcode, body)).is_err() {
            break;
        }
        if let Some((opcode, body)) = reply {
            let frame = encode_frame(opcode, body.to_string().as_bytes());
            if stream.write_all(&frame).is_err() || reject {
                break;
            }
        }
    }
    let _ = stream.shutdown(Shutdown::Both);
}

/// A cut-down copy of what Discord serves at `/applications/detectable`.
pub const DETECTABLE_SAMPLE: &str = r#"[
  {"aliases": [], "executables": [{"is_launcher": false, "name": "binaries/victoria3.exe", "os": "win32"}],
   "hook": true, "id": "1124352195157045371", "name": "Victoria 3", "overlay": false,
   "third_party_skus": [{"distributor": "steam", "id": "529340"}]},
  {"aliases": ["ELDEN RING"], "executables": [], "id": "1402418436809953330", "name": "ELDEN RING",
   "third_party_skus": [{"distributor": "xbox", "id": "9P3J32CTXLRZ"}, {"distributor": "steam", "id": "1245620"}]},
  {"id": "111", "name": "Stray whitespace", "third_party_skus": [{"distributor": "steam", "id": "207490\t"}]},
  {"id": "222", "name": "First", "third_party_skus": [{"distributor": "steam", "id": "777"}]},
  {"id": "333", "name": "Second", "third_party_skus": [{"distributor": "steam", "id": "777"}]},
  {"id": "444", "name": "Xbox only", "third_party_skus": [{"distributor": "xbox", "id": "5"}]},
  {"id": "555", "name": "Null SKU", "third_party_skus": [{"distributor": "steam", "id": null}]},
  {"id": "not-a-number", "name": "Bad ID", "third_party_skus": [{"distributor": "steam", "id": "888"}]},
  {"id": "666", "name": "No SKUs"}
]"#;

/// A fetcher that always gives the same answer and counts its calls.
pub fn counting_fetcher(result: Result<&'static str, &'static str>) -> (Fetcher, Arc<AtomicUsize>) {
    let calls = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&calls);
    let fetch = Box::new(move || {
        counter.fetch_add(1, Ordering::SeqCst);
        match result {
            Ok(json) => Ok(json.as_bytes().to_vec()),
            Err(why) => Err(anyhow::anyhow!(why)),
        }
    });
    (fetch, calls)
}
