mod auth;
mod browser;
mod callback;
mod commands;
mod keys;
mod media;

use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use async_trait::async_trait;
use matrix_sdk::{Client, config::SyncSettings};
use messages_core::{Capabilities, Capability, Conversation, Encryption, RoomKey, RoomKind};
use messages_protocols::bridge::{API_VERSION, BridgeStatus};
use serde_json::{Value, json};
use tokio::sync::{Mutex, Notify, RwLock};

use crate::{
    Backend, Command,
    cache::{Cache, SharedCache},
};

pub struct Matrix {
    client: Client,
    client_uri: Option<String>,
    cache: SharedCache,
    directory: PathBuf,
    ready: Arc<Notify>,
    decryption_needed: Notify,
    writes: Arc<Mutex<()>>,
    file_lock: Arc<std::sync::Mutex<()>>,
    committed: Arc<std::sync::atomic::AtomicBool>,
    browser: Arc<Mutex<browser::Browser>>,
    methods: Mutex<Option<(std::time::Instant, auth::Methods)>>,
    problem: RwLock<bool>,
}

fn capabilities() -> Capabilities {
    Capabilities::new([
        Capability::StartConversation,
        Capability::SendText,
        Capability::Reply,
        Capability::React,
        Capability::EditOwn,
        Capability::DeleteOwn,
        Capability::Typing,
        Capability::ReadReceipts,
        Capability::AttachFiles,
    ])
}

pub(super) async fn save_private(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let path = path.to_owned();
    let bytes = bytes.to_vec();
    tokio::task::spawn_blocking(move || auth::save_private_sync(&path, &bytes)).await?
}

impl Matrix {
    pub async fn prepare(directory: &Path, homeserver: &str) -> anyhow::Result<Arc<Self>> {
        Self::prepare_with_client_uri(directory, homeserver, None).await
    }

    /// Prepare an isolated account with its application's HTTPS OAuth identity.
    /// Without an explicit address, use the identity supplied when building the helper.
    pub async fn prepare_with_client_uri(
        directory: &Path,
        homeserver: &str,
        client_uri: Option<&str>,
    ) -> anyhow::Result<Arc<Self>> {
        let client_uri = client_uri
            .or(option_env!("MESSAGES_MATRIX_CLIENT_URI"))
            .map(str::to_owned);
        let key_path = directory.join("database-key");
        let key = match tokio::fs::read_to_string(&key_path).await {
            Ok(key) => key,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                let key = format!(
                    "{}{}",
                    uuid::Uuid::new_v4().simple(),
                    uuid::Uuid::new_v4().simple()
                );
                save_private(&key_path, key.as_bytes()).await?;
                key
            }
            Err(error) => return Err(error.into()),
        };
        let client = Client::builder()
            .homeserver_url(homeserver)
            .sqlite_store(directory.join("sdk"), Some(&key))
            .handle_refresh_tokens()
            .with_encryption_settings(matrix_sdk::encryption::EncryptionSettings {
                backup_download_strategy:
                    matrix_sdk::encryption::BackupDownloadStrategy::AfterDecryptionFailure,
                ..Default::default()
            })
            .build()
            .await?;
        let file_lock = Arc::new(std::sync::Mutex::new(()));
        auth::session_callbacks(&client, directory.join("session.json"), file_lock.clone())?;
        match auth::load_session(&directory.join("session.json")) {
            Ok(session) => client.restore_session(session).await?,
            Err(error)
                if error
                    .downcast_ref::<std::io::Error>()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) => {}
            Err(error) => return Err(error),
        }
        let committed = Arc::new(std::sync::atomic::AtomicBool::new(
            client.user_id().is_some(),
        ));
        Ok(Arc::new(Self {
            client,
            client_uri,
            cache: Cache::open(directory.join("messages.json")).await?,
            directory: directory.into(),
            ready: Arc::new(Notify::new()),
            decryption_needed: Notify::new(),
            writes: Arc::new(Mutex::new(())),
            file_lock,
            committed,
            browser: Arc::new(Mutex::new(browser::Browser::default())),
            methods: Mutex::new(None),
            problem: RwLock::new(false),
        }))
    }

    pub async fn run(
        self: Arc<Self>,
        mut shutdown: tokio::sync::watch::Receiver<bool>,
    ) -> anyhow::Result<()> {
        loop {
            let notified = self.ready.notified();
            if self.committed.load(std::sync::atomic::Ordering::Acquire) {
                break;
            }
            tokio::select! { _ = notified => {}, _ = shutdown.changed() => return Ok(()) }
        }
        tokio::select! {
            result = self.sync(shutdown.clone()) => result,
            result = self.receive_keys(shutdown) => result,
        }
    }

    async fn sync(&self, mut shutdown: tokio::sync::watch::Receiver<bool>) -> anyhow::Result<()> {
        let mut cursor = match tokio::fs::read_to_string(self.directory.join("sync-cursor")).await {
            Ok(cursor) => Some(cursor),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        loop {
            let mut settings = SyncSettings::default().timeout(Duration::from_secs(15));
            if let Some(cursor) = &cursor {
                settings = settings.token(cursor);
            }
            let response = tokio::select! {
                _ = shutdown.changed() => break,
                response = self.client.sync_once(settings) => response,
            };
            match response {
                Ok(response) => {
                    for (id, update) in response.rooms.joined {
                        let Some(room) = self.client.get_room(&id) else {
                            continue;
                        };
                        let key = RoomKey::new(uuid::Uuid::nil(), id.as_str());
                        let title = room.display_name().await?.to_string();
                        let encryption = if room.encryption_state().is_encrypted() {
                            Encryption::EndToEnd
                        } else {
                            Encryption::Transport
                        };
                        let direct = room.is_direct().await?;
                        let mut cache = self.cache.lock().await;
                        if cache.store.room(&key).is_none() {
                            cache.store.conversations.push(Conversation {
                                key: key.clone(),
                                title: title.clone(),
                                subtitle: "Matrix".into(),
                                kind: if direct {
                                    RoomKind::Direct
                                } else {
                                    RoomKind::Group
                                },
                                members: Vec::new(),
                                unread: 0,
                                pinned: false,
                                muted: false,
                                muted_until: None,
                                avatar_revision: 0,
                                group_name: None,
                                parent_room: None,
                                archived: false,
                                encryption,
                                capabilities: capabilities(),
                                messages: Vec::new(),
                                draft: String::new(),
                                draft_attachments: Vec::new(),
                                reply_to: None,
                                has_older: true,
                                pagination: update.timeline.prev_batch.clone(),
                            });
                        }
                        if let Some(existing) = cache.store.room_mut(&key) {
                            existing.title = title;
                            existing.encryption = encryption;
                        }
                        let own = self.client.user_id().map(|id| id.as_str()).unwrap_or("");
                        let mut encrypted_events = false;
                        for event in update.timeline.events {
                            let event: Value = serde_json::from_str(event.raw().json().get())?;
                            encrypted_events |= event["type"] == "m.room.encrypted";
                            cache.apply_matrix(&key, &event, own)?;
                        }
                        for event in update.ephemeral {
                            let event: Value = serde_json::from_str(event.json().get())?;
                            if event["type"] == "m.typing" {
                                let active: std::collections::BTreeSet<_> =
                                    event["content"]["user_ids"]
                                        .as_array()
                                        .into_iter()
                                        .flatten()
                                        .filter_map(Value::as_str)
                                        .filter(|id| *id != own)
                                        .collect();
                                let old: Vec<_> = cache
                                    .typing
                                    .keys()
                                    .filter(|(chat, _)| chat == &key.room)
                                    .map(|(_, actor)| actor.clone())
                                    .collect();
                                for actor in old {
                                    if !active.contains(actor.as_str()) {
                                        cache.set_typing(key.room.clone(), actor, false);
                                    }
                                }
                                for actor in active {
                                    cache.set_typing(key.room.clone(), actor.into(), true);
                                }
                            } else if event["type"] == "m.receipt"
                                && let Some(receipts) = event["content"].as_object()
                            {
                                for (id, receipt) in receipts {
                                    if receipt["m.read"].get(own).is_some()
                                        || receipt["m.read.private"].get(own).is_some()
                                    {
                                        let through = cache
                                            .store
                                            .room(&key)
                                            .and_then(|r| r.messages.iter().find(|m| &m.id == id))
                                            .map(|m| m.timestamp.timestamp_millis());
                                        if let Some(through) = through {
                                            cache.store.mark_chat_read(&key, through);
                                        }
                                    }
                                }
                            }
                        }
                        cache.persist().await?;
                        if encrypted_events {
                            self.decryption_needed.notify_one();
                        }
                    }
                    save_private(
                        &self.directory.join("sync-cursor"),
                        response.next_batch.as_bytes(),
                    )
                    .await?;
                    cursor = Some(response.next_batch);
                    *self.problem.write().await = false;
                }
                Err(_) => {
                    *self.problem.write().await = true;
                    tokio::select! { _ = shutdown.changed() => break, _ = tokio::time::sleep(Duration::from_secs(10)) => {} }
                }
            }
        }
        Ok(())
    }
}

#[async_trait]
impl Backend for Matrix {
    async fn status(&self) -> BridgeStatus {
        BridgeStatus {
            api_version: API_VERSION,
            protocol: "matrix".into(),
            authenticated: self.committed.load(std::sync::atomic::Ordering::Acquire),
            user_id: self.client.user_id().map(ToString::to_string),
            capabilities: capabilities(),
            login_state: Some(
                if *self.problem.read().await {
                    "Connection issue"
                } else if self.committed.load(std::sync::atomic::Ordering::Acquire) {
                    "Connected"
                } else {
                    "Sign-in required"
                }
                .into(),
            ),
        }
    }
    async fn login(&self) -> Value {
        if self.committed.load(std::sync::atomic::Ordering::Acquire) {
            if self.client.encryption().recovery().state()
                == matrix_sdk::encryption::recovery::RecoveryState::Incomplete
            {
                return json!({"state":"matrix_recovery"});
            }
            return json!({"state":"ready"});
        }
        let state = self.browser.lock().await;
        if let Some(pending) = state.status() {
            return pending;
        }
        let problem = state.problem();
        drop(state);
        match self.methods().await {
            Ok(methods) => {
                json!({"state":"matrix","password":methods.password,"browser":methods.legacy||methods.oauth,"problem":problem})
            }
            Err(_) => {
                json!({"state":"unsupported","reason":"Could not discover a supported sign-in method. Check the homeserver address and refresh."})
            }
        }
    }
    async fn browser_start(&self, request: uuid::Uuid) -> anyhow::Result<Value> {
        self.start_browser(request).await
    }
    async fn browser_cancel(&self, request: uuid::Uuid) -> anyhow::Result<()> {
        self.cancel_browser(request).await
    }

    async fn authenticate(&self, input: Value) -> anyhow::Result<()> {
        let _write = self.writes.lock().await;
        if input["operation"] == "recover" {
            anyhow::ensure!(
                self.committed.load(std::sync::atomic::Ordering::Acquire),
                "Sign in before restoring encryption keys"
            );
            let key = input["value"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("Missing recovery key"))?;
            self.client.encryption().recovery().recover(key).await.map_err(|_| anyhow::anyhow!("Could not restore encryption keys. Check your recovery key or passphrase and connection, then try again."))?;
            self.decryption_needed.notify_one();
            return Ok(());
        }
        anyhow::ensure!(
            self.client.oauth().client_id().is_none(),
            "Use browser sign-in for this account"
        );
        let user = input["user"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing user ID"))?;
        let password = input["value"]
            .as_str()
            .ok_or_else(|| anyhow::anyhow!("Missing password"))?;
        self.client
            .matrix_auth()
            .login_username(user, password)
            .initial_device_display_name("enzUI Messages")
            .await?;
        auth::save_session(
            &self.client,
            &self.directory.join("session.json"),
            &self.file_lock,
        )?;
        self.committed
            .store(true, std::sync::atomic::Ordering::Release);
        self.ready.notify_one();
        Ok(())
    }
    async fn command(&self, operation: &str, command: Command) -> anyhow::Result<Value> {
        self.dispatch(operation, command).await
    }
    fn cache(&self) -> &SharedCache {
        &self.cache
    }
}
