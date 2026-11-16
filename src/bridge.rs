//! Passes a game's own Rich Presence on to Discord.
//!
//! Many Windows games tell Discord what is happening in the game ("Playing
//! as Belgium, 1853") through the named pipe `\\.\pipe\discord-ipc-0`.
//! Inside a Proton prefix that pipe does not exist, so the information is
//! lost. Two pieces fix that:
//!
//! - a small Windows helper (`bridge/bridge.c`) that the daemon starts
//!   inside the game's Wine prefix. It creates the pipe and relays every
//!   connection to the daemon over TCP on 127.0.0.1;
//! - [`BridgeServer`], which accepts those connections and relays them to
//!   Discord's Unix socket, watching for the game's `SET_ACTIVITY`.
//!
//! While the game publishes an activity of its own, the daemon withdraws
//! its generic one, so Discord shows exactly what the game says.

use std::collections::hash_map::RandomState;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::hash::{BuildHasher, Hasher};
use std::io::{self, Read, Write};
use std::net::{Ipv4Addr, Shutdown, TcpListener, TcpStream};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use log::{debug, info, warn};
use serde_json::Value;

use crate::discord::{self, decode_frame, OP_CLOSE, OP_FRAME, OP_HANDSHAKE};

/// Tells the helper where the daemon is (`<port>:<token>.<launch>`), and
/// marks the helper's processes so the game scan ignores them.
pub const ENV_VAR: &str = "PROTON_PRESENCE_BRIDGE";
pub const HELPER_NAME: &str = "proton-presence-bridge.exe";

const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_HELLO: usize = 200;

/// Variables Wine sets for its own child processes. They describe the
/// process they were made for and must not leak into a new one.
const WINE_PRIVATE_VARS: &[&str] = &["WINELOADERNOEXEC", "WINESERVERSOCKET", "WINEPRELOADRESERVE"];

/// Namespaces the helper has to share with the game, and the `nsenter`
/// options that enter them.
const NAMESPACES: &[(&str, &[&str])] = &[
    ("user", &["--user", "--preserve-credentials"]),
    ("mnt", &["--mount"]),
    ("pid", &["--pid"]),
    ("ipc", &["--ipc"]),
    ("net", &["--net"]),
];

struct Shared {
    token: String,
    socket_dirs: Vec<PathBuf>,
    /// Relayed game connections that currently hold an activity.
    active: AtomicUsize,
    /// Control connections of running helpers.
    controls: Mutex<Vec<TcpStream>>,
    /// Number of the next helper launch.
    next_launch: AtomicU64,
    /// Launches below this number have been dismissed. A helper from one
    /// of them is turned away even if it only connects afterwards.
    first_valid: AtomicU64,
}

/// The daemon's end of the bridge: a TCP listener on 127.0.0.1.
pub struct BridgeServer {
    port: u16,
    shared: Arc<Shared>,
}

impl BridgeServer {
    /// Starts listening. `socket_dirs` are searched for Discord's socket.
    pub fn start(socket_dirs: Vec<PathBuf>) -> io::Result<BridgeServer> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))?;
        let port = listener.local_addr()?.port();
        let shared = Arc::new(Shared {
            token: random_token(),
            socket_dirs,
            active: AtomicUsize::new(0),
            controls: Mutex::new(Vec::new()),
            next_launch: AtomicU64::new(0),
            first_valid: AtomicU64::new(0),
        });

        let accept_shared = Arc::clone(&shared);
        thread::Builder::new()
            .name("bridge-accept".into())
            .spawn(move || {
                for stream in listener.incoming().flatten() {
                    let shared = Arc::clone(&accept_shared);
                    // A failed spawn just drops this one connection.
                    let _ = thread::Builder::new()
                        .name("bridge-conn".into())
                        .spawn(move || handle(stream, &shared));
                }
            })?;

        Ok(BridgeServer { port, shared })
    }

    /// The value of [`ENV_VAR`] for a new helper launch:
    /// `<port>:<token>.<launch>`.
    pub fn launch_env(&self) -> String {
        let launch = self.shared.next_launch.fetch_add(1, Ordering::SeqCst);
        format!("{}{launch}", self.env_prefix())
    }

    /// What the [`ENV_VAR`] value of every helper of this daemon starts
    /// with.
    pub fn env_prefix(&self) -> String {
        format!("{}:{}.", self.port, self.shared.token)
    }

    /// Whether the game is publishing an activity of its own right now.
    pub fn game_presence_active(&self) -> bool {
        self.shared.active.load(Ordering::SeqCst) > 0
    }

    /// Whether a helper is running and connected.
    pub fn helper_connected(&self) -> bool {
        !self.shared.controls.lock().unwrap().is_empty()
    }

    /// Tells every helper launched so far to exit, by closing its control
    /// connection now or as soon as it connects.
    pub fn disconnect_helpers(&self) {
        // Under the lock, so no helper can register in between.
        let mut controls = self.shared.controls.lock().unwrap();
        let launched = self.shared.next_launch.load(Ordering::SeqCst);
        self.shared.first_valid.store(launched, Ordering::SeqCst);
        for control in controls.drain(..) {
            let _ = control.shutdown(Shutdown::Both);
        }
    }
}

/// 128 random bits as hex. `RandomState` is seeded by the operating system.
fn random_token() -> String {
    let word = || RandomState::new().build_hasher().finish();
    format!("{:016x}{:016x}", word(), word())
}

/// Reads the helper's first line, `<kind> <token>.<launch>\n`, and returns
/// the kind and the launch number.
fn read_hello(stream: &mut TcpStream, token: &str) -> Option<(u8, u64)> {
    let mut line = Vec::new();
    let mut byte = [0u8; 1];
    while line.len() < MAX_HELLO {
        match stream.read(&mut byte) {
            Ok(1) if byte[0] == b'\n' => {
                let (kind, rest) = line.split_first()?;
                let launch = rest
                    .strip_prefix(b" ")?
                    .strip_prefix(token.as_bytes())?
                    .strip_prefix(b".")?;
                let launch = std::str::from_utf8(launch).ok()?.parse().ok()?;
                return Some((*kind, launch));
            }
            Ok(1) => line.push(byte[0]),
            _ => return None,
        }
    }
    None
}

fn handle(mut stream: TcpStream, shared: &Shared) {
    let _ = stream.set_read_timeout(Some(HELLO_TIMEOUT));
    let Some((kind, launch)) = read_hello(&mut stream, &shared.token) else {
        debug!("Bridge: rejected a connection without a valid token");
        return;
    };
    let _ = stream.set_read_timeout(None);
    match kind {
        b'C' => control(stream, shared, launch),
        b'D' if launch >= shared.first_valid.load(Ordering::SeqCst) => relay(stream, shared),
        _ => {}
    }
}

/// Holds a helper's control connection until either side closes it.
fn control(mut stream: TcpStream, shared: &Shared, launch: u64) {
    let Ok(peer) = stream.peer_addr() else { return };
    {
        // Checked under the lock that `disconnect_helpers` takes, so a
        // helper is either registered before a dismissal and closed by
        // it, or sees the dismissal here.
        let mut controls = shared.controls.lock().unwrap();
        if launch < shared.first_valid.load(Ordering::SeqCst) {
            debug!("Bridge: turned away a helper that was already dismissed");
            return;
        }
        match stream.try_clone() {
            Ok(clone) => controls.push(clone),
            Err(_) => return,
        }
    }
    debug!("Bridge: helper connected");
    let mut byte = [0u8; 1];
    while matches!(stream.read(&mut byte), Ok(n) if n > 0) {}
    shared
        .controls
        .lock()
        .unwrap()
        .retain(|c| c.peer_addr().ok() != Some(peer));
    debug!("Bridge: helper disconnected");
}

/// Relays one game connection to Discord and back.
fn relay(mut game: TcpStream, shared: &Shared) {
    // If Discord is closed the connection is simply dropped; the game's
    // Discord library retries on its own.
    let Some((mut discord, _)) = discord::connect_any(&shared.socket_dirs) else {
        debug!("Bridge: the game looked for Discord, but Discord is not running");
        return;
    };
    let (Ok(mut to_game), Ok(mut from_discord)) = (game.try_clone(), discord.try_clone()) else {
        return;
    };
    let replies = thread::spawn(move || {
        let mut frames = FrameSplitter::default();
        let mut chunk = [0u8; 8192];
        loop {
            let n = match from_discord.read(&mut chunk) {
                Ok(n) if n > 0 => n,
                _ => break,
            };
            if to_game.write_all(&chunk[..n]).is_err() {
                break;
            }
            for (opcode, body) in frames.feed(&chunk[..n]) {
                log_reply(opcode, &body);
            }
        }
        let _ = to_game.shutdown(Shutdown::Both);
    });

    let mut tracker = ActivityTracker::default();
    let mut chunk = [0u8; 8192];
    loop {
        let n = match game.read(&mut chunk) {
            Ok(n) if n > 0 => n,
            _ => break,
        };
        if discord.write_all(&chunk[..n]).is_err() {
            break;
        }
        match tracker.feed(&chunk[..n]) {
            Some(true) => shared.active.fetch_add(1, Ordering::SeqCst),
            Some(false) => shared.active.fetch_sub(1, Ordering::SeqCst),
            None => 0,
        };
    }
    debug!("Bridge: the game's connection to Discord closed");
    if tracker.active {
        shared.active.fetch_sub(1, Ordering::SeqCst);
    }
    let _ = discord.shutdown(Shutdown::Both);
    let _ = game.shutdown(Shutdown::Both);
    let _ = replies.join();
}

/// Logs what Discord answers the game, so a presence that does not show
/// up can be traced.
fn log_reply(opcode: u32, body: &Value) {
    let message = body["data"]["message"]
        .as_str()
        .or(body["message"].as_str())
        .unwrap_or("no reason given");
    match opcode {
        OP_CLOSE => warn!("Discord refused the game's connection: {message}"),
        OP_FRAME if body["evt"] == "ERROR" => warn!(
            "Discord rejected the game's {} request: {message}",
            body["cmd"].as_str().unwrap_or("unknown")
        ),
        OP_FRAME if body["evt"] == "READY" => debug!("Bridge: Discord accepted the game"),
        _ => {}
    }
}

/// Cuts a relayed byte stream into Discord IPC frames for inspection.
#[derive(Default)]
struct FrameSplitter {
    buf: Vec<u8>,
    /// Set when the stream stops looking like Discord IPC. The bytes are
    /// still relayed; they are just no longer inspected.
    lost: bool,
}

impl FrameSplitter {
    /// Takes the next bytes and returns the frames they complete.
    fn feed(&mut self, bytes: &[u8]) -> Vec<(u32, Value)> {
        let mut frames = Vec::new();
        if self.lost {
            return frames;
        }
        self.buf.extend_from_slice(bytes);
        loop {
            match decode_frame(&self.buf) {
                Ok(Some((opcode, payload, used))) => {
                    self.buf.drain(..used);
                    let body = serde_json::from_slice(&payload).unwrap_or(Value::Null);
                    frames.push((opcode, body));
                }
                Ok(None) => break,
                Err(_) => {
                    self.lost = true;
                    self.buf = Vec::new();
                    break;
                }
            }
        }
        frames
    }
}

/// Follows the frames a game sends to see whether it holds an activity.
#[derive(Default)]
pub struct ActivityTracker {
    frames: FrameSplitter,
    active: bool,
}

impl ActivityTracker {
    /// Takes the next bytes from the game. Returns `Some(active)` when the
    /// game set (`true`) or cleared (`false`) its activity.
    pub fn feed(&mut self, bytes: &[u8]) -> Option<bool> {
        let before = self.active;
        for (opcode, body) in self.frames.feed(bytes) {
            match opcode {
                OP_HANDSHAKE => info!(
                    "The game opened its own connection to Discord (application {})",
                    body["client_id"].as_str().unwrap_or("unknown")
                ),
                OP_FRAME if body["cmd"] == "SET_ACTIVITY" => {
                    self.active = body["args"]["activity"].is_object();
                }
                OP_FRAME => debug!(
                    "Bridge: the game sent {}",
                    body["cmd"].as_str().unwrap_or("an unknown command")
                ),
                _ => {}
            }
        }
        (self.active != before).then_some(self.active)
    }
}

/// How to start the helper inside a running game's Wine prefix.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Launch {
    pub program: PathBuf,
    pub args: Vec<OsString>,
    pub env: Vec<(OsString, OsString)>,
}

/// Starts and stops helper processes. A trait so that tests do not run
/// or kill anything.
pub trait HelperRunner {
    fn launch(&mut self, launch: &Launch) -> io::Result<Option<Child>>;
    /// Forcibly ends processes that did not exit when asked to.
    fn kill(&mut self, pids: &[u32]);
}

/// The real [`HelperRunner`].
pub struct SystemRunner;

impl HelperRunner for SystemRunner {
    fn launch(&mut self, launch: &Launch) -> io::Result<Option<Child>> {
        spawn_helper(launch)
    }

    fn kill(&mut self, pids: &[u32]) {
        let result = Command::new("kill")
            .arg("-KILL")
            .args(pids.iter().map(u32::to_string))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
        if let Err(e) = result {
            warn!("Cannot run kill to stop a stuck bridge helper: {e}");
        }
    }
}

/// Works out the command that runs the helper next to a game.
///
/// The helper must join the game's `wineserver`, so it gets the same Wine
/// binary and environment the game was started with, both read from the
/// `wineserver` process. Steam usually runs games in a container; where
/// the game's namespaces differ from ours, `nsenter` enters them first.
pub fn plan_launch(
    proc_root: &Path,
    wineserver_pid: u32,
    helper: &Path,
    env_value: &str,
) -> Result<Launch> {
    let proc_dir = proc_root.join(wineserver_pid.to_string());
    let server = fs::read_link(proc_dir.join("exe")).context("cannot read the wineserver path")?;
    let bin_dir = server
        .parent()
        .context("wineserver path has no directory")?;
    let is_wineserver = server
        .file_name()
        .and_then(OsStr::to_str)
        .is_some_and(|name| name.starts_with("wineserver"));
    if !is_wineserver {
        bail!("process {wineserver_pid} is not a wineserver");
    }

    // `wine64` where it exists, otherwise `wine`. The path is as the game
    // sees it, so look through the game's root directory as well.
    let seen_by_game = |path: &Path| {
        let relative = path.strip_prefix("/").unwrap_or(path);
        proc_dir.join("root").join(relative).is_file() || path.is_file()
    };
    let wine = ["wine64", "wine"]
        .iter()
        .map(|name| bin_dir.join(name))
        .find(|path| seen_by_game(path))
        .with_context(|| format!("no wine binary in {}", bin_dir.display()))?;

    let environ = fs::read(proc_dir.join("environ")).context("cannot read the Wine environment")?;
    let mut env: Vec<(OsString, OsString)> = environ
        .split(|&b| b == 0)
        .filter_map(|entry| {
            let eq = entry.iter().position(|&b| b == b'=')?;
            let key = OsStr::from_bytes(&entry[..eq]);
            let overridden = key == ENV_VAR || key == "WINEDEBUG";
            let private = WINE_PRIVATE_VARS.iter().any(|var| key == *var);
            (eq > 0 && !overridden && !private).then(|| {
                (
                    key.to_owned(),
                    OsStr::from_bytes(&entry[eq + 1..]).to_owned(),
                )
            })
        })
        .collect();
    if !env.iter().any(|(key, _)| key == "WINEPREFIX") {
        bail!("the wineserver environment has no WINEPREFIX");
    }
    env.push((ENV_VAR.into(), env_value.into()));
    env.push(("WINEDEBUG".into(), "-all".into()));

    let mut ns_options: Vec<&str> = Vec::new();
    for (namespace, options) in NAMESPACES {
        let theirs = fs::read_link(proc_dir.join("ns").join(namespace));
        let ours = fs::read_link(proc_root.join("self/ns").join(namespace));
        if let (Ok(theirs), Ok(ours)) = (theirs, ours) {
            if theirs != ours {
                ns_options.extend_from_slice(options);
            }
        }
    }

    if ns_options.is_empty() {
        return Ok(Launch {
            program: wine,
            args: vec![helper.into()],
            env,
        });
    }
    let mut args: Vec<OsString> = vec!["--target".into(), wineserver_pid.to_string().into()];
    args.extend(ns_options.iter().map(OsString::from));
    args.extend(["--".into(), wine.into(), helper.into()]);
    Ok(Launch {
        program: nsenter_path(),
        args,
        env,
    })
}

/// `nsenter` by absolute path: the helper's environment comes from the
/// game, so its `PATH` cannot be relied on to find host tools.
fn nsenter_path() -> PathBuf {
    ["/usr/bin/nsenter", "/bin/nsenter", "/usr/sbin/nsenter"]
        .iter()
        .map(PathBuf::from)
        .find(|path| path.is_file())
        .unwrap_or_else(|| PathBuf::from("nsenter"))
}

/// Runs the helper command, detached from our terminal.
fn spawn_helper(launch: &Launch) -> io::Result<Option<Child>> {
    Command::new(&launch.program)
        .args(&launch.args)
        .env_clear()
        .envs(launch.env.iter().map(|(k, v)| (k, v)))
        .current_dir("/")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::discord::{encode_frame, read_frame, socket_dirs, OP_HANDSHAKE};
    use crate::testutil::{add_proc, FakeDiscord, TempDir};
    use serde_json::json;
    use std::os::unix::fs::symlink;

    fn frame(opcode: u32, body: Value) -> Vec<u8> {
        encode_frame(opcode, body.to_string().as_bytes())
    }

    fn set_activity(activity: Value) -> Vec<u8> {
        frame(
            OP_FRAME,
            json!({ "cmd": "SET_ACTIVITY", "args": { "pid": 1, "activity": activity }, "nonce": "n" }),
        )
    }

    /// Connects like the helper does and sends its first line.
    fn helper_connect(server: &BridgeServer, kind: char) -> TcpStream {
        helper_connect_as(&server.launch_env(), kind)
    }

    /// Connects with the environment value of one particular launch.
    fn helper_connect_as(value: &str, kind: char) -> TcpStream {
        let (port, token) = {
            let (port, token) = value.split_once(':').unwrap();
            (port.parse::<u16>().unwrap(), token.to_string())
        };
        let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
        stream
            .write_all(format!("{kind} {token}\n").as_bytes())
            .unwrap();
        stream
    }

    /// Polls until `condition` holds; the bridge runs on other threads.
    fn eventually(what: &str, condition: impl Fn() -> bool) {
        for _ in 0..500 {
            if condition() {
                return;
            }
            thread::sleep(Duration::from_millis(10));
        }
        panic!("timed out waiting until {what}");
    }

    #[test]
    fn tracker_follows_set_and_clear() {
        let mut tracker = ActivityTracker::default();
        assert_eq!(tracker.feed(&frame(OP_HANDSHAKE, json!({ "v": 1 }))), None);
        assert_eq!(
            tracker.feed(&set_activity(json!({ "details": "x" }))),
            Some(true)
        );
        assert_eq!(
            tracker.feed(&set_activity(json!({ "details": "y" }))),
            None,
            "an update is not a change of state"
        );
        assert_eq!(tracker.feed(&set_activity(Value::Null)), Some(false));
        let other = frame(
            OP_FRAME,
            json!({ "cmd": "SUBSCRIBE", "evt": "ACTIVITY_JOIN" }),
        );
        assert_eq!(tracker.feed(&other), None);
    }

    #[test]
    fn tracker_reassembles_frames_split_across_reads() {
        let mut tracker = ActivityTracker::default();
        let mut bytes = frame(OP_HANDSHAKE, json!({ "v": 1 }));
        bytes.extend(set_activity(json!({ "state": "1853" })));
        let (last, rest) = bytes.split_last().unwrap();
        for byte in rest {
            assert_eq!(tracker.feed(&[*byte]), None);
        }
        assert_eq!(tracker.feed(&[*last]), Some(true));
    }

    #[test]
    fn tracker_gives_up_on_a_stream_that_is_not_discord_ipc() {
        let mut tracker = ActivityTracker::default();
        assert_eq!(tracker.feed(&[0xff; 64]), None);
        assert_eq!(tracker.feed(&set_activity(json!({ "details": "x" }))), None);
    }

    #[test]
    fn tokens_are_random() {
        let (a, b) = (random_token(), random_token());
        assert_eq!(a.len(), 32);
        assert_ne!(a, b);
    }

    #[test]
    fn relays_a_game_connection_to_discord_and_tracks_its_activity() {
        let tmp = TempDir::new("bridge");
        let discord = FakeDiscord::start(&tmp.path().join("discord-ipc-0"));
        let server = BridgeServer::start(socket_dirs(tmp.path())).unwrap();
        assert!(!server.game_presence_active());

        let mut game = helper_connect(&server, 'D');
        let handshake = json!({ "v": 1, "client_id": "424242" });
        game.write_all(&frame(OP_HANDSHAKE, handshake.clone()))
            .unwrap();
        assert_eq!(discord.recv(), (OP_HANDSHAKE, handshake));

        // Discord's reply travels back to the game.
        let mut buf = Vec::new();
        let (opcode, payload) = read_frame(&mut game, &mut buf).unwrap();
        let reply: Value = serde_json::from_slice(&payload).unwrap();
        assert_eq!((opcode, &reply["evt"]), (OP_FRAME, &json!("READY")));

        let activity = json!({ "details": "Playing as Belgium", "state": "1853" });
        game.write_all(&set_activity(activity.clone())).unwrap();
        let (_, body) = discord.recv();
        assert_eq!(body["args"]["activity"], activity);
        eventually("the activity is tracked", || server.game_presence_active());

        game.write_all(&set_activity(Value::Null)).unwrap();
        discord.recv();
        eventually("the cleared activity is tracked", || {
            !server.game_presence_active()
        });
    }

    #[test]
    fn activity_ends_when_the_game_disconnects() {
        let tmp = TempDir::new("bridge");
        let discord = FakeDiscord::start(&tmp.path().join("discord-ipc-0"));
        let server = BridgeServer::start(socket_dirs(tmp.path())).unwrap();

        let mut game = helper_connect(&server, 'D');
        game.write_all(&frame(OP_HANDSHAKE, json!({ "v": 1, "client_id": "1" })))
            .unwrap();
        game.write_all(&set_activity(json!({ "details": "x" })))
            .unwrap();
        discord.recv();
        discord.recv();
        eventually("the activity is tracked", || server.game_presence_active());

        drop(game);
        eventually("the activity is gone", || !server.game_presence_active());
    }

    #[test]
    fn activity_ends_when_discord_quits() {
        let tmp = TempDir::new("bridge");
        let discord = FakeDiscord::start(&tmp.path().join("discord-ipc-0"));
        let server = BridgeServer::start(socket_dirs(tmp.path())).unwrap();

        let mut game = helper_connect(&server, 'D');
        game.write_all(&frame(OP_HANDSHAKE, json!({ "v": 1, "client_id": "1" })))
            .unwrap();
        game.write_all(&set_activity(json!({ "details": "x" })))
            .unwrap();
        eventually("the activity is tracked", || server.game_presence_active());

        discord.stop();
        eventually("the activity is gone", || !server.game_presence_active());
        // The game's side is closed too, so its library can reconnect.
        let mut rest = Vec::new();
        game.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        assert!(game.read_to_end(&mut rest).is_ok());
    }

    #[test]
    fn drops_the_game_connection_while_discord_is_closed() {
        let tmp = TempDir::new("bridge");
        let server = BridgeServer::start(socket_dirs(tmp.path())).unwrap();
        let mut game = helper_connect(&server, 'D');
        game.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let mut rest = Vec::new();
        assert_eq!(game.read_to_end(&mut rest).unwrap(), 0);
        assert!(!server.game_presence_active());
    }

    #[test]
    fn rejects_connections_without_the_token() {
        let tmp = TempDir::new("bridge");
        let discord = FakeDiscord::start(&tmp.path().join("discord-ipc-0"));
        let server = BridgeServer::start(socket_dirs(tmp.path())).unwrap();
        let value = server.launch_env();
        let port: u16 = value.split(':').next().unwrap().parse().unwrap();
        let token = value.split_once(':').unwrap().1.split('.').next().unwrap();
        let no_launch = format!("D {token}\n");
        let bad_launch = format!("D {token}.x\n");

        for hello in [
            "D wrong-token.0\n",
            &no_launch,
            &bad_launch,
            "D\n",
            "\n",
            "X",
        ] {
            let mut stream = TcpStream::connect((Ipv4Addr::LOCALHOST, port)).unwrap();
            stream.write_all(hello.as_bytes()).unwrap();
            let _ = stream.write_all(&set_activity(json!({ "details": "x" })));
            if hello.ends_with('\n') {
                stream
                    .set_read_timeout(Some(Duration::from_secs(5)))
                    .unwrap();
                let mut rest = Vec::new();
                let _ = stream.read_to_end(&mut rest);
                assert!(rest.is_empty(), "{hello:?}");
            }
        }
        assert_eq!(discord.try_recv(Duration::from_millis(100)), None);
        assert!(!server.game_presence_active());
        assert!(!server.helper_connected());
    }

    #[test]
    fn control_connection_tracks_the_helper_and_can_dismiss_it() {
        let tmp = TempDir::new("bridge");
        let server = BridgeServer::start(socket_dirs(tmp.path())).unwrap();
        assert!(!server.helper_connected());

        let mut control = helper_connect(&server, 'C');
        eventually("the helper is registered", || server.helper_connected());

        // Closing the control connection is how the helper is told to exit.
        server.disconnect_helpers();
        control
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut rest = Vec::new();
        assert_eq!(control.read_to_end(&mut rest).unwrap(), 0);
        eventually("the helper is forgotten", || !server.helper_connected());

        // A helper that exits by itself is forgotten as well.
        let control = helper_connect(&server, 'C');
        eventually("the helper is registered", || server.helper_connected());
        drop(control);
        eventually("the helper is forgotten", || !server.helper_connected());
    }

    #[test]
    fn a_dismissed_helper_is_turned_away_even_if_it_connects_late() {
        let tmp = TempDir::new("bridge");
        let discord = FakeDiscord::start(&tmp.path().join("discord-ipc-0"));
        let server = BridgeServer::start(socket_dirs(tmp.path())).unwrap();

        // Launched, but dismissed before it got round to connecting.
        let late = server.launch_env();
        server.disconnect_helpers();

        let mut control = helper_connect_as(&late, 'C');
        control
            .set_read_timeout(Some(Duration::from_secs(5)))
            .unwrap();
        let mut rest = Vec::new();
        assert_eq!(control.read_to_end(&mut rest).unwrap(), 0, "closed at once");
        assert!(!server.helper_connected());

        // Its game connections are closed too (as a reset, if what the
        // game sent was never read) and nothing reaches Discord.
        let mut game = helper_connect_as(&late, 'D');
        let _ = game.write_all(&set_activity(json!({ "details": "x" })));
        game.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        match game.read_to_end(&mut rest) {
            Ok(n) => assert_eq!(n, 0),
            Err(e) => assert_eq!(e.kind(), io::ErrorKind::ConnectionReset),
        }
        assert_eq!(discord.try_recv(Duration::from_millis(100)), None);
        assert!(!server.game_presence_active());

        // A helper launched after the dismissal is welcome.
        let _control = helper_connect(&server, 'C');
        eventually("the new helper is registered", || server.helper_connected());
    }

    /// A fake `/proc` entry for a wineserver, plus this process's own.
    fn fake_wineserver(tmp: &TempDir, pid: u32, exe: &Path, container: bool) -> PathBuf {
        let proc_root = tmp.path().join("proc");
        add_proc(
            &proc_root,
            pid,
            "wineserver",
            &[exe.to_str().unwrap()],
            &[
                ("SteamAppId", "529340"),
                ("WINEPREFIX", "/games/compatdata/529340/pfx"),
                ("WINEFSYNC", "1"),
                ("WINEDEBUG", "+all"),
                ("WINELOADERNOEXEC", "1"),
                ("WINESERVERSOCKET", "7"),
            ],
        );
        let theirs = proc_root.join(pid.to_string());
        let ours = proc_root.join("self");
        symlink(exe, theirs.join("exe")).unwrap();
        fs::create_dir_all(theirs.join("ns")).unwrap();
        fs::create_dir_all(ours.join("ns")).unwrap();
        for (namespace, _) in NAMESPACES {
            // May already exist from an earlier call in the same test.
            let _ = symlink(format!("{namespace}:[1]"), ours.join("ns").join(namespace));
            // Steam's container has its own user, mount and PID namespaces.
            let id = match (container, *namespace) {
                (true, "user" | "mnt" | "pid") => 2,
                _ => 1,
            };
            symlink(
                format!("{namespace}:[{id}]"),
                theirs.join("ns").join(namespace),
            )
            .unwrap();
        }
        proc_root
    }

    fn wine_dir(tmp: &TempDir, binaries: &[&str]) -> PathBuf {
        let dir = tmp.path().join("proton/files/bin");
        fs::create_dir_all(&dir).unwrap();
        for name in binaries {
            fs::write(dir.join(name), "").unwrap();
        }
        dir
    }

    fn env_of<'a>(launch: &'a Launch, key: &str) -> Option<&'a str> {
        launch
            .env
            .iter()
            .find(|(k, _)| k == key)
            .and_then(|(_, v)| v.to_str())
    }

    #[test]
    fn plans_a_direct_launch_when_the_game_is_not_in_a_container() {
        let tmp = TempDir::new("launch");
        let bin = wine_dir(&tmp, &["wineserver", "wine", "wine64"]);
        let proc_root = fake_wineserver(&tmp, 500, &bin.join("wineserver"), false);
        let helper = Path::new("/home/u/.local/share/proton-presence/proton-presence-bridge.exe");

        let launch = plan_launch(&proc_root, 500, helper, "4242:secret").unwrap();
        assert_eq!(launch.program, bin.join("wine64"));
        assert_eq!(launch.args, [OsString::from(helper)]);

        // The game's Wine settings are kept; per-process ones are not.
        assert_eq!(
            env_of(&launch, "WINEPREFIX"),
            Some("/games/compatdata/529340/pfx")
        );
        assert_eq!(env_of(&launch, "WINEFSYNC"), Some("1"));
        assert_eq!(env_of(&launch, "SteamAppId"), Some("529340"));
        assert_eq!(env_of(&launch, "WINELOADERNOEXEC"), None);
        assert_eq!(env_of(&launch, "WINESERVERSOCKET"), None);
        assert_eq!(env_of(&launch, ENV_VAR), Some("4242:secret"));
        assert_eq!(env_of(&launch, "WINEDEBUG"), Some("-all"));
        assert_eq!(
            launch.env.iter().filter(|(k, _)| k == "WINEDEBUG").count(),
            1
        );
    }

    #[test]
    fn plans_nsenter_into_steams_container() {
        let tmp = TempDir::new("launch");
        let bin = wine_dir(&tmp, &["wineserver", "wine"]);
        let proc_root = fake_wineserver(&tmp, 500, &bin.join("wineserver"), true);
        let helper = Path::new("/h/proton-presence-bridge.exe");

        let launch = plan_launch(&proc_root, 500, helper, "1:t").unwrap();
        assert_eq!(launch.program.file_name().unwrap(), "nsenter");
        let args: Vec<&str> = launch.args.iter().map(|a| a.to_str().unwrap()).collect();
        let wine = bin.join("wine");
        assert_eq!(
            args,
            [
                "--target",
                "500",
                "--user",
                "--preserve-credentials",
                "--mount",
                "--pid",
                "--",
                wine.to_str().unwrap(),
                "/h/proton-presence-bridge.exe",
            ]
        );
    }

    #[test]
    fn refuses_to_plan_without_a_usable_wine() {
        let tmp = TempDir::new("launch");
        let helper = Path::new("/h/bridge.exe");

        let bin = wine_dir(&tmp, &["wineserver"]);
        let proc_root = fake_wineserver(&tmp, 500, &bin.join("wineserver"), false);
        let err = plan_launch(&proc_root, 500, helper, "1:t").unwrap_err();
        assert!(err.to_string().contains("no wine binary"), "{err}");

        assert!(
            plan_launch(&proc_root, 501, helper, "1:t").is_err(),
            "no such process"
        );

        let proc_root = fake_wineserver(&tmp, 502, &bin.join("bash"), false);
        let err = plan_launch(&proc_root, 502, helper, "1:t").unwrap_err();
        assert!(err.to_string().contains("not a wineserver"), "{err}");
    }
}
