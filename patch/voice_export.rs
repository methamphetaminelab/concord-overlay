use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, OnceLock, RwLock};
use std::time::Duration;

use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Notify, mpsc};

use crate::discord::{AppEvent, DiscordState};
use crate::logging;

const TARGET: &str = "voice_export";
const SOCKET_NAME: &str = "concord-voice.sock";
const QUEUE_LEN: usize = 16;
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);

static EXPORT: OnceLock<Option<Arc<Notify>>> = OnceLock::new();

pub(crate) fn on_app_event(state: &Arc<RwLock<DiscordState>>, event: &AppEvent) {
    if cfg!(test) {
        return;
    }
    let Some(dirty) = EXPORT.get_or_init(|| start(state)) else {
        return;
    };
    if is_voice_relevant(event) {
        dirty.notify_one();
    }
}

#[allow(clippy::unneeded_struct_pattern)]
fn is_voice_relevant(event: &AppEvent) -> bool {
    matches!(
        event,
        AppEvent::Ready { .. }
            | AppEvent::ReadySnapshotComplete { .. }
            | AppEvent::SignedOut { .. }
            | AppEvent::UserIdentityUpdate { .. }
            | AppEvent::GuildCreate { .. }
            | AppEvent::GuildUpdate { .. }
            | AppEvent::GuildDelete { .. }
            | AppEvent::GuildMemberAdd { .. }
            | AppEvent::GuildMemberUpsert { .. }
            | AppEvent::GuildMemberRemove { .. }
            | AppEvent::GuildMembersChunk { .. }
            | AppEvent::ChannelUpsert { .. }
            | AppEvent::ChannelDelete { .. }
            | AppEvent::ChannelRecipientAdd { .. }
            | AppEvent::ChannelRecipientRemove { .. }
            | AppEvent::LazyPrivateChannelUpsert { .. }
            | AppEvent::VoiceStateUpdate { .. }
            | AppEvent::VoiceSpeakingUpdate { .. }
            | AppEvent::CallDelete { .. }
    )
}

fn start(state: &Arc<RwLock<DiscordState>>) -> Option<Arc<Notify>> {
    if std::env::var_os("CONCORD_VOICE_EXPORT").is_some_and(|v| v == "0") {
        logging::debug(TARGET, "disabled by CONCORD_VOICE_EXPORT=0");
        return None;
    }
    let Some(dir) = std::env::var_os("XDG_RUNTIME_DIR").filter(|d| !d.is_empty()) else {
        logging::error(TARGET, "XDG_RUNTIME_DIR is not set; voice export disabled");
        return None;
    };
    let handle = match tokio::runtime::Handle::try_current() {
        Ok(handle) => handle,
        Err(error) => {
            logging::error(TARGET, format!("no tokio runtime ({error}); voice export disabled"));
            return None;
        }
    };
    let dirty = Arc::new(Notify::new());
    handle.spawn(run_server(
        PathBuf::from(dir).join(SOCKET_NAME),
        Arc::clone(state),
        Arc::clone(&dirty),
    ));
    Some(dirty)
}

async fn run_server(path: PathBuf, state: Arc<RwLock<DiscordState>>, dirty: Arc<Notify>) {
    let listener = match bind(&path).await {
        Ok(listener) => listener,
        Err(message) => {
            logging::error(TARGET, format!("{message}; voice export disabled"));
            return;
        }
    };
    logging::debug(TARGET, format!("listening on {}", path.display()));
    let mut hub = Broadcaster::default();
    loop {
        tokio::select! {
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    let (tx, rx) = mpsc::channel(QUEUE_LEN);
                    tokio::spawn(serve_client(stream, rx));
                    let Some(line) = state_line(&state) else { return };
                    hub.add_client(tx, line);
                }
                Err(error) => {
                    logging::error(TARGET, format!("accept failed: {error}"));
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
            },
            () = dirty.notified() => {
                if hub.clients.is_empty() {
                    continue;
                }
                let Some(line) = state_line(&state) else { return };
                hub.publish(line);
            }
        }
    }
}

async fn bind(path: &Path) -> Result<UnixListener, String> {
    let shown = path.display();
    if path.exists() {
        if UnixStream::connect(path).await.is_ok() {
            return Err(format!("{shown} is in use by another concord"));
        }
        std::fs::remove_file(path).map_err(|e| format!("cannot remove stale {shown}: {e}"))?;
    }
    let listener = UnixListener::bind(path).map_err(|e| format!("cannot bind {shown}: {e}"))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .map_err(|e| format!("cannot chmod 0600 {shown}: {e}"))?;
    Ok(listener)
}

async fn serve_client(stream: UnixStream, mut rx: mpsc::Receiver<Arc<str>>) {
    let (mut reader, mut writer) = stream.into_split();
    let mut discard = [0u8; 256];
    loop {
        tokio::select! {
            line = rx.recv() => {
                let Some(line) = line else { return };
                match tokio::time::timeout(WRITE_TIMEOUT, writer.write_all(line.as_bytes())).await {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => {
                        logging::debug(TARGET, format!("client write failed: {error}"));
                        return;
                    }
                    Err(_) => {
                        logging::error(TARGET, "client stopped reading; dropped");
                        return;
                    }
                }
            }
            read = reader.read(&mut discard) => match read {
                Ok(0) => return,
                Ok(_) => {}
                Err(error) => {
                    logging::debug(TARGET, format!("client read failed: {error}"));
                    return;
                }
            }
        }
    }
}

#[derive(Default)]
struct Broadcaster {
    last: Option<Arc<str>>,
    clients: Vec<mpsc::Sender<Arc<str>>>,
}

impl Broadcaster {
    fn publish(&mut self, line: String) {
        if self.last.as_deref() == Some(line.as_str()) {
            return;
        }
        let line: Arc<str> = line.into();
        self.clients.retain(|tx| try_send(tx, &line));
        self.last = Some(line);
    }

    fn add_client(&mut self, tx: mpsc::Sender<Arc<str>>, line: String) {
        match &self.last {
            Some(last) if **last == *line => {
                if try_send(&tx, last) {
                    self.clients.push(tx);
                }
            }
            _ => {
                self.clients.push(tx);
                self.publish(line);
            }
        }
    }
}

fn try_send(tx: &mpsc::Sender<Arc<str>>, line: &Arc<str>) -> bool {
    match tx.try_send(Arc::clone(line)) {
        Ok(()) => true,
        Err(mpsc::error::TrySendError::Full(_)) => {
            logging::error(TARGET, "client queue full; dropped");
            false
        }
        Err(mpsc::error::TrySendError::Closed(_)) => false,
    }
}

fn state_line(state: &RwLock<DiscordState>) -> Option<String> {
    let view = match state.read() {
        Ok(state) => compute_view(&state),
        Err(_) => {
            logging::error(TARGET, "discord state lock poisoned; voice export stopped");
            return None;
        }
    };
    match serde_json::to_string(&view) {
        Ok(mut line) => {
            line.push('\n');
            Some(line)
        }
        Err(error) => {
            logging::error(TARGET, format!("serialization failed: {error}; voice export stopped"));
            None
        }
    }
}

#[derive(Serialize)]
struct View {
    v: u32,
    self_id: Option<String>,
    channel: Option<Channel>,
    members: Vec<Member>,
}

#[derive(Serialize)]
struct Channel {
    id: String,
    name: String,
    guild_id: Option<String>,
}

#[derive(Serialize)]
struct Member {
    id: String,
    name: String,
    avatar: Option<String>,
    speaking: bool,
    mute: bool,
    deaf: bool,
    self_mute: bool,
    self_deaf: bool,
    stream: bool,
}

fn compute_view(state: &DiscordState) -> View {
    let self_id = state.current_user_id().map(|id| id.to_string());
    let Some(conn) = state.current_user_voice_connection() else {
        return View { v: 1, self_id, channel: None, members: Vec::new() };
    };
    let channel_id = conn.channel_id;
    let guild_id = conn.guild_id();
    let channel = state.channel(channel_id);
    let mut participants = match guild_id {
        Some(guild_id) => state.voice_participants_for_channel(guild_id, channel_id),
        None => state.voice_participants_for_private_channel(channel_id),
    };
    participants.sort_by_key(|p| p.user_id.get());
    let members = participants
        .into_iter()
        .map(|p| {
            let avatar = match guild_id {
                Some(guild_id) => state
                    .member_for_guild(guild_id, p.user_id)
                    .and_then(|m| m.avatar_url.clone()),
                None => channel
                    .and_then(|c| c.recipients.iter().find(|r| r.user_id == p.user_id))
                    .and_then(|r| r.avatar_url.clone()),
            };
            Member {
                id: p.user_id.to_string(),
                name: p.display_name,
                avatar,
                speaking: p.speaking,
                mute: p.mute,
                deaf: p.deaf,
                self_mute: p.self_mute,
                self_deaf: p.self_deaf,
                stream: p.self_stream,
            }
        })
        .collect();
    View {
        v: 1,
        self_id,
        channel: Some(Channel {
            id: channel_id.to_string(),
            name: channel.map_or_else(|| format!("channel-{channel_id}"), |c| c.name.clone()),
            guild_id: guild_id.map(|id| id.to_string()),
        }),
        members,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(view: &View) -> String {
        serde_json::to_string(view).expect("view serializes")
    }

    fn member(id: &str, name: &str) -> Member {
        Member {
            id: id.to_owned(),
            name: name.to_owned(),
            avatar: None,
            speaking: false,
            mute: false,
            deaf: false,
            self_mute: false,
            self_deaf: false,
            stream: false,
        }
    }

    #[test]
    fn empty_state_serializes_nulls() {
        let view = compute_view(&DiscordState::default());
        assert_eq!(line(&view), r#"{"v":1,"self_id":null,"channel":null,"members":[]}"#);
    }

    #[test]
    fn view_serializes_to_one_line_with_string_ids() {
        let mut me = member("100000000000000001", "a \"quoted\"\nname 🎧");
        me.avatar = Some("https://cdn.discordapp.com/avatars/1/a.png".to_owned());
        me.speaking = true;
        let view = View {
            v: 1,
            self_id: Some("100000000000000001".to_owned()),
            channel: Some(Channel {
                id: "200000000000000002".to_owned(),
                name: "Общий 🔊".to_owned(),
                guild_id: None,
            }),
            members: vec![me],
        };
        let text = line(&view);
        assert!(!text.contains('\n'), "must stay one line: {text}");
        assert!(text.contains(r#""self_id":"100000000000000001""#));
        assert!(text.contains(r#""guild_id":null"#));
        assert!(text.contains(r#""name":"a \"quoted\"\nname 🎧""#));
        let parsed: serde_json::Value = serde_json::from_str(&text).expect("valid json");
        assert_eq!(parsed["members"][0]["name"], "a \"quoted\"\nname 🎧");
        assert_eq!(parsed["members"][0]["id"], "100000000000000001");
        assert_eq!(parsed["members"][0]["speaking"], true);
        assert_eq!(parsed["members"][0]["stream"], false);
        assert_eq!(parsed["channel"]["name"], "Общий 🔊");
    }

    #[test]
    fn identical_state_is_not_resent() {
        let mut hub = Broadcaster::default();
        let (tx, mut rx) = mpsc::channel(QUEUE_LEN);
        hub.add_client(tx, "A\n".to_owned());
        hub.publish("A\n".to_owned());
        hub.publish("B\n".to_owned());
        hub.publish("B\n".to_owned());
        assert_eq!(&*rx.try_recv().expect("A"), "A\n");
        assert_eq!(&*rx.try_recv().expect("B"), "B\n");
        assert!(rx.try_recv().is_err(), "duplicates must not be sent");

        let (tx2, mut rx2) = mpsc::channel(QUEUE_LEN);
        hub.add_client(tx2, "B\n".to_owned());
        assert_eq!(&*rx2.try_recv().expect("B for newcomer"), "B\n");
        assert!(rx.try_recv().is_err());

        let (tx3, mut rx3) = mpsc::channel(QUEUE_LEN);
        hub.add_client(tx3, "C\n".to_owned());
        for rx in [&mut rx, &mut rx2, &mut rx3] {
            assert_eq!(&*rx.try_recv().expect("C"), "C\n");
        }
    }

    #[test]
    fn slow_client_is_dropped_without_affecting_others() {
        let mut hub = Broadcaster::default();
        let (slow_tx, mut slow_rx) = mpsc::channel(QUEUE_LEN);
        let (fast_tx, mut fast_rx) = mpsc::channel(QUEUE_LEN);
        hub.add_client(slow_tx, "0\n".to_owned());
        hub.add_client(fast_tx, "0\n".to_owned());
        assert_eq!(&*fast_rx.try_recv().expect("initial"), "0\n");
        for i in 1..=QUEUE_LEN {
            hub.publish(format!("{i}\n"));
            assert_eq!(&*fast_rx.try_recv().expect("fast client keeps up"), format!("{i}\n"));
        }
        assert_eq!(hub.clients.len(), 1, "slow client dropped once its queue is full");
        hub.publish("last\n".to_owned());
        assert_eq!(&*fast_rx.try_recv().expect("still served"), "last\n");
        let mut received = 0;
        while slow_rx.try_recv().is_ok() {
            received += 1;
        }
        assert_eq!(received, QUEUE_LEN);
        assert!(slow_rx.is_closed());
    }

    #[tokio::test]
    async fn serve_client_writes_lines_and_ends_when_dropped() {
        let (server, mut client) = UnixStream::pair().expect("socket pair");
        let (tx, rx) = mpsc::channel(QUEUE_LEN);
        let task = tokio::spawn(serve_client(server, rx));
        tx.send(Arc::from("{\"v\":1}\n")).await.expect("queued");
        drop(tx);
        let mut got = String::new();
        client.read_to_string(&mut got).await.expect("read until EOF");
        assert_eq!(got, "{\"v\":1}\n");
        task.await.expect("writer task ends");
    }

    #[tokio::test]
    async fn bind_sets_0600_refuses_live_socket_and_replaces_stale_one() {
        let dir = std::env::temp_dir().join(format!("voice-export-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        let path = dir.join(SOCKET_NAME);
        let _ = std::fs::remove_file(&path);

        let live = bind(&path).await.expect("first bind");
        let mode = std::fs::metadata(&path).expect("socket exists").permissions().mode();
        assert_eq!(mode & 0o777, 0o600);
        let err = bind(&path).await.expect_err("live socket must be refused");
        assert!(err.contains("in use"), "{err}");

        drop(live);
        assert!(path.exists());
        let _rebound = bind(&path).await.expect("stale socket replaced");

        std::fs::remove_dir_all(&dir).expect("cleanup");
    }
}
