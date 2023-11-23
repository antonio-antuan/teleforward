use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::str::FromStr;
use std::sync::{Arc, OnceLock};
use std::{env, fs, path};

use anyhow::{Context, Result};
use async_trait::async_trait;
use chrono::prelude::*;
use clap::{Args, Parser, Subcommand};
use log::LevelFilter;
use rust_tdlib::client::auth_handler::ClientAuthStateHandler;
use rust_tdlib::client::tdlib_client::TdJson;
use rust_tdlib::client::{AuthStateHandlerProxy, ClientIdentifier, ClientState};
use rust_tdlib::types::{AuthorizationState, AuthorizationStateWaitCode, AuthorizationStateWaitPassword, AuthorizationStateWaitPhoneNumber, AuthorizationStateWaitRegistration, ChatType, CreatePrivateChat, DownloadFile, GetChat, GetChatHistory, GetMessageLink, GetSupergroup, GetUser, Message, MessageOrigin, RObject, Usernames};
use rust_tdlib::types::{FormattedText, GetMe, MessageContent, TextEntity, TextEntityType};
use rust_tdlib::{
    client::{Client, Worker},
    tdjson,
    types::{SetTdlibParameters, Update},
};
use serde::Deserialize;
use thiserror::Error;
use tokio::sync::mpsc::{Receiver, Sender};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

#[derive(Parser)]
struct Cli {
    #[arg(short, long, default_value = "config.yml")]
    config: String,
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Initialize clients
    Init(Input),
    /// Runs the main routine
    Run,
    /// Synchronizes history
    Sync,
}

#[derive(Args)]
struct Input {
    phones_to_codes: Option<String>,
}

static ACCOUNTS_DATA: OnceLock<HashMap<i32, ClientWithMeta>> = OnceLock::new();

#[derive(Debug)]
struct ClientWithMeta {
    chat_id: i64,
    file: Arc<Mutex<File>>,
    client: Client<TdJson>,
    file_path: String,
}

#[derive(Debug, Deserialize)]
struct Config {
    accounts: Vec<AccountSettings>,
    telegram: TelegramConfig,

    #[serde(default = "default_loglevel")]
    log_level: String,
}

#[derive(Debug, Deserialize)]
struct TelegramConfig {
    #[serde(default = "default_verbosity")]
    tdlib_log_verbosity: i32,

    api_id: i32,
    api_hash: String,
}

const fn default_verbosity() -> i32 {
    1
}

fn default_loglevel() -> String {
    "error".to_string()
}

#[derive(Deserialize, Debug)]
struct AccountSettings {
    phone: String,
    tddb_dir: String,
    file_path: String,
}

#[derive(Debug, Clone)]
struct ClientAuthorizer {
    phone: String,
    auth_code: Option<String>,
}

#[async_trait]
impl ClientAuthStateHandler for ClientAuthorizer {
    async fn handle_wait_code(&self, _wait_code: &AuthorizationStateWaitCode) -> String {
        match &self.auth_code {
            None => {
                log::warn!("auth code is needed for {}", self.phone);
                "".to_string()
            }
            Some(code) => code.clone(),
        }
    }

    async fn handle_wait_password(
        &self,
        _wait_password: &AuthorizationStateWaitPassword,
    ) -> String {
        unimplemented!("password is not supported")
    }

    async fn handle_wait_client_identifier(
        &self,
        _: &AuthorizationStateWaitPhoneNumber,
    ) -> ClientIdentifier {
        ClientIdentifier::PhoneNumber(self.phone.clone())
    }

    async fn handle_wait_registration(
        &self,
        _wait_registration: &AuthorizationStateWaitRegistration,
    ) -> (String, String) {
        unimplemented!("registration is not supported")
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let f = File::open(cli.config).expect("Could not open file.");
    let config: Config = serde_yaml::from_reader(f).expect("Could not read values.");

    setup_logging(&config.log_level, config.telegram.tdlib_log_verbosity).context("logging setup")?;

    let (sender, receiver) = tokio::sync::mpsc::channel::<Box<Update>>(100);

    let reader = create_updates_reader(receiver);

    let mut worker = Worker::builder()
        .with_auth_state_handler(AuthStateHandlerProxy::default())
        .build().context("cannot initialize telegram worker")?;
    let waiter = worker.start();

    match &cli.command {
        Commands::Init(arg) => {
            let mut codes = HashMap::new();
            arg.phones_to_codes.as_ref().map(|s| {
                for line in s.lines() {
                    let mut split = line.splitn(2, ':');
                    let phone = split.next().unwrap().to_string();
                    let code = split.next().unwrap().to_string();
                    codes.insert(phone, code);
                }
            });

            let auth_resp = auth_clients(config, &mut worker, sender.clone(), codes).await;
            worker.stop();
            waiter.await?;

            match auth_resp {
                Ok(_) => {}
                Err(err) => match err.downcast_ref::<AppError>() {
                    None => {}
                    Some(app_err) => match app_err {
                        AppError::WaitCode => {
                            log::warn!("code is needed");
                        }
                        _ => Err(err).context("authorization failed")?,
                    },
                },
            };
        }
        Commands::Run => {
            auth_clients(config, &mut worker, sender.clone(), HashMap::new()).await.context("cannot authorize clients")?;
            tokio::select! {
                _ = waiter => {log::warn!("worker stopped")}
                res = reader => {
                    match res {
                        Ok(_) => {
                            log::info!("reader stopped");
                        }
                        Err(err) => {
                            log::error!("reader error: {}", err);
                        }
                    }
                }
                _ = tokio::signal::ctrl_c() => {log::info!("ctrl-c received")}
            }
        }
        Commands::Sync => {
            for account in config.accounts.iter() {
                let acc_data = setup_client(
                    &mut worker,
                    account,
                    config.telegram.api_id,
                    config.telegram.api_hash.clone(),
                    None,
                    None,
                )
                .await.context(format!("{} client authorization", &account.phone))?;
                sync(&acc_data).await.context(format!("sync {}", &account.phone))?;
            }
        }
    }

    Ok(())
}

async fn auth_clients(
    config: Config,
    worker: &mut Worker<AuthStateHandlerProxy, TdJson>,
    sender: Sender<Box<Update>>,
    codes: HashMap<String, String>,
) -> Result<()> {
    let mut accounts_data = HashMap::new();

    for account in config.accounts.iter() {
        let code = codes.get(&account.phone);
        let client_data = setup_client(
            worker,
            account,
            config.telegram.api_id,
            config.telegram.api_hash.clone(),
            Some(sender.clone()),
            code,
        )
        .await.context(format!("setup client {}", &account.phone))?;

        accounts_data.insert(
            client_data
                .client
                .get_client_id()
                .expect("client_id not set"),
            client_data,
        );
    }
    ACCOUNTS_DATA
        .set(accounts_data)
        .expect("accounts data already set");
    Ok(())
}

fn setup_logging(log_level: &str, tdlib_log_verbosity: i32) -> Result<()> {
    env_logger::Builder::new()
        .filter_level(LevelFilter::from_str("warn").context("parse loglevel")?)
        .filter_module(env!("CARGO_PKG_NAME"), LevelFilter::from_str(&log_level).context("parse loglevel")?)
        .init();
    tdjson::set_log_verbosity_level(tdlib_log_verbosity);
    Ok(())
}

async fn setup_client(
    worker: &mut Worker<AuthStateHandlerProxy, TdJson>,
    account: &AccountSettings,
    api_id: i32,
    api_hash: String,
    sender: Option<Sender<Box<Update>>>,
    auth_code: Option<&String>,
) -> Result<ClientWithMeta> {
    let mut builder = Client::builder()
        .with_tdlib_parameters(
            SetTdlibParameters::builder()
                .database_directory(&account.tddb_dir)
                .use_test_dc(false)
                .api_id(api_id)
                .api_hash(api_hash)
                .system_language_code("en")
                .device_model("Desktop")
                .system_version("Unknown")
                .application_version(env!("CARGO_PKG_VERSION"))
                .enable_storage_optimizer(true)
                .build(),
        )
        .with_auth_state_channel(10)
        .with_client_auth_state_handler(ClientAuthorizer {
            phone: account.phone.clone(),
            auth_code: auth_code.map(|s| s.clone()),
        });
    match sender {
        None => {}
        Some(sender) => {
            builder = builder.with_updates_sender(sender);
        }
    }
    let client = builder.build().context("client parameters setup")?;
    let client = worker.bind_client(client).await.context("bind client to worker")?;
    wait_authorized(&client, &worker).await.context("wait authorized")?;
    log::debug!("{} authorized", account.phone);

    let me = client.get_me(GetMe::builder().build()).await.context("telegram:get_me")?;
    log::debug!("authorized as: {:?}", me);

    let path = path::Path::new(account.file_path.as_str());
    fs::create_dir_all(path.parent().unwrap()).context("create data dir")?;
    let file = OpenOptions::new()
        .write(true)
        .append(true)
        .create(true)
        .open(account.file_path.as_str()).context("open data file")?;

    Ok(ClientWithMeta {
        chat_id: me.id(),
        file: Arc::new(Mutex::new(file)),
        client,
        file_path: account.file_path.clone(),
    })
}

async fn sync(acc_data: &ClientWithMeta) -> Result<()> {
    // TODO: create backup
    log::info!("start sync for client {}", acc_data.chat_id);
    let mut from_msg_id = 0;
    acc_data
        .client
        .create_private_chat(
            CreatePrivateChat::builder()
                .user_id(acc_data.chat_id)
                .build(),
        )
        .await.context(r#"get "SavedMessages" chat"#)?;
    let mut total_processed_messages = 0;
    loop {
        let messages = acc_data
            .client
            .get_chat_history(
                GetChatHistory::builder()
                    .chat_id(acc_data.chat_id)
                    .offset(0)
                    .from_message_id(from_msg_id)
                    .limit(100),
            )
            .await.context("telegram:get_chat_history")?;
        if messages.total_count() == 0 {
            log::debug!("processed {} messages", total_processed_messages);
            return Ok(());
        }

        for msg in messages.messages() {
            match msg {
                None => {}
                Some(msg) => {
                    total_processed_messages += 1;
                    from_msg_id = msg.id();
                    process_message(msg, acc_data).await.context("process message")?;
                }
            }
        }
        log::info!("processed {} messages", total_processed_messages,);
    }
}

#[derive(Error, Debug)]
enum AppError {
    #[error("wait code")]
    WaitCode,
    #[error("tdlib error")]
    TdlibError(#[from] rust_tdlib::errors::Error),
}

async fn wait_authorized(
    client: &Client<TdJson>,
    worker: &Worker<AuthStateHandlerProxy, TdJson>,
) -> Result<(), AppError> {
    loop {
        match worker.wait_auth_state_change(&client).await.context("worker:wait_auth_state_change") {
            Ok(res) => match res {
                Ok(state) => match state {
                    ClientState::Opened => {
                        log::debug!("client authorized; can start interaction");
                        break;
                    }
                    _ => {
                        panic!("client not authorized: {:?}", state);
                    }
                },
                Err((err, auth_state)) => {
                    return match auth_state.authorization_state() {
                        AuthorizationState::WaitCode(_) => Err(AppError::WaitCode)?,
                        _ => Err(AppError::TdlibError(err))?,
                    }
                }
            },
            Err(err) => {
                panic!("cannot wait for auth state changes: {}", err);
            }
        }
    }
    Ok(())
}

fn create_updates_reader(mut receiver: Receiver<Box<Update>>) -> JoinHandle<Result<()>> {
    tokio::spawn(async move {
        while let Some(message) = receiver.recv().await {
            match message.as_ref() {
                Update::NewMessage(new_message) => match ACCOUNTS_DATA.get() {
                    None => {
                        log::debug!("accounts data is not set");
                        continue;
                    }
                    Some(d) => {
                        let client_id = new_message.client_id().unwrap_or(-1);
                        match d.get(&client_id) {
                            None => {
                                log::error!("client_id not found: {}", client_id);
                                continue;
                            }
                            Some(data) => {
                                log::trace!("chat_id of message: {}, expected chat_id: {}", new_message.message().chat_id(), &data.chat_id);
                                if &new_message.message().chat_id() != &data.chat_id {
                                    continue;
                                }
                                process_message(new_message.message(), data).await.context("process message")?;
                            }
                        }
                    }
                },
                _ => {}
            }
        }
        Ok(())
    })
}

async fn process_message(message: &Message, client_meta: &ClientWithMeta) -> Result<()> {
    log::trace!("message content: {:?}", message);
    // TODO: if a message contains more than one photo - actually there are several messages with the same media_album_id.
    // we need kind of debounce here
    match parse_message_content(client_meta, message.content()).await {
        Some(text) => {
            let message_meta = match get_message_meta(message, client_meta).await {
                Ok(m) => m,
                Err(err) => {
                    log::error!("cannot get message meta: {}", err);
                    return Ok(());
                }
            };
            let mut text = match message_meta.message_link {
                Some(link) => {
                    format!(
                        r#"
**Date:** [{message_date}]({message_link})

{text}

---

"#,
                        message_date = message_meta.message_date.format("%Y-%m-%d %H:%M:%S"),
                        message_link = link,
                        text = text
                    )
                },
                None => {
                    format!(
                        r#"
**Date:** {message_date}

{text}

---

"#,
                        message_date = message_meta.message_date.format("%Y-%m-%d %H:%M:%S"),
                        text = text
                    )
                }
            };
            if let Some(n) = message_meta.channel_name {
                text = format!("**From:** {}\n\n{}", n, text);
            }
            client_meta.file.lock().await.write_all(text.as_bytes()).context("write to file")?;
        }
        None => {}
    };
    Ok(())
}

struct MessageMeta {
    channel_name: Option<String>,
    message_link: Option<String>,
    message_date: NaiveDateTime,
}

async fn get_message_meta(message: &Message, client_meta: &ClientWithMeta) -> Result<MessageMeta> {
    let (channel_name, link_request) = match message.forward_info() {
        None => {
            if message.chat_id() == client_meta.chat_id {
                return Ok(MessageMeta {
                    channel_name: Some("SavedMessages".to_string()),
                    message_link: None,
                    message_date: NaiveDateTime::from_timestamp_opt(message.date() as i64, 0)
                        .context("cannot parse message date")?,
                });
            }
            let chat = client_meta
                .client
                .get_chat(GetChat::builder().chat_id(message.chat_id()).build())
                .await.context("telegram:get_chat")?;
            let name = match chat.type_() {
                ChatType::Supergroup(sg) => {
                    let sg = client_meta
                        .client
                        .get_supergroup(GetSupergroup::builder().supergroup_id(sg.supergroup_id()))
                        .await.context("telegram:get_supergroup")?;
                    get_username(sg.usernames())
                }
                _ => None,
            };
            (
                name,
                Some(GetMessageLink::builder()
                    .chat_id(message.chat_id())
                    .message_id(message.id())
                    .build())
            )
        }
        Some(forward_info) => {
            match forward_info.origin() {
                MessageOrigin::_Default => {
                    unreachable!("default message origin is not supported")
                }
                MessageOrigin::Channel(channel) => {
                    (
                        get_channel_name(client_meta, channel.chat_id()).await.context("telegram:get channel name")?,
                        Some(GetMessageLink::builder()
                            .chat_id(forward_info.from_chat_id())
                            .message_id(forward_info.from_message_id())
                            .build())
                    )
                }
                MessageOrigin::Chat(chat) => {
                    (
                        get_channel_name(client_meta, chat.sender_chat_id()).await.context("telegram:get chat name")?,
                        None,
                    )
                }
                MessageOrigin::HiddenUser(hu) => {
                    (
                        Some(hu.sender_name().clone()),
                        None,
                    )
                }
                MessageOrigin::User(user) => {
                    let user = client_meta.client.get_user(GetUser::builder().user_id(user.sender_user_id()).build()).await.context("telegram:get_user")?;
                    (
                        get_username(user.usernames()),
                        None,
                    )
                }
            }
        }
    };

    let mut link = None;
    if let Some(link_request) = link_request {
        let link_resp = client_meta.client.get_message_link(link_request).await;
        if let Ok(resp) = link_resp {
            link = Some(resp.link().clone())
        }
    };

    Ok(MessageMeta {
        channel_name,
        message_link: link,
        message_date: NaiveDateTime::from_timestamp_opt(message.date() as i64, 0)
            .context("cannot parse message date")?,
    })
}

async fn get_channel_name(client_meta: &ClientWithMeta, chat_id: i64) -> Result<Option<String>> {
    let chat = client_meta
        .client
        .get_chat(
            GetChat::builder()
                .chat_id(chat_id)
                .build(),
        )
        .await.context("telegram:get_chat")?;
    Ok(match chat.type_() {
        ChatType::_Default => {
            unreachable!("chat type is not supported")
        }
        ChatType::Supergroup(sg) => {
            let sg = client_meta
                .client
                .get_supergroup(GetSupergroup::builder().supergroup_id(sg.supergroup_id()))
                .await.context("telegram:get_supergroup")?;
            get_username(sg.usernames())
        }
        ChatType::BasicGroup(_) => None,
        ChatType::Private(pr) => {
            let u = client_meta
                .client
                .get_user(GetUser::builder().user_id(pr.user_id()).build())
                .await.context("telegram:get_user")?;
            get_username(u.usernames())
        }
        ChatType::Secret(_) => {
            unimplemented!("secret chat is not supported")
        }
    })
}
fn get_username(usernames: &Option<Usernames>) -> Option<String> {
    usernames
        .iter()
        .next()
        .map(|s| s.active_usernames().first().unwrap().clone())
}

async fn parse_message_content(
    client_meta: &ClientWithMeta,
    content: &MessageContent,
) -> Option<String> {
    match content {
        MessageContent::MessageText(text) => return Some(parse_formatted_text(text.text())),
        MessageContent::MessageAnimation(message_animation) => {
            return Some(parse_formatted_text(message_animation.caption()));
        }
        MessageContent::MessageAudio(message_audio) => {
            return Some(parse_formatted_text(message_audio.caption()));
        }
        MessageContent::MessageDocument(message_document) => {
            let doc = message_document.document();
            log::info!("downloading file: {}", doc.file_name());
            let file = match client_meta
                .client
                .download_file(
                    DownloadFile::builder()
                        .file_id(doc.document().id())
                        .synchronous(true)
                        .priority(1)
                        .build(),
                )
                .await
            {
                Ok(file) => file,
                Err(err) => {
                    log::error!("cannot download file: {}", err);
                    return None;
                }
            };
            log::info!("downloaded file: {}", doc.file_name());
            let path = path::Path::new(client_meta.file_path.as_str())
                .parent()?
                .join(doc.file_name());
            fs::rename(file.local().path(), &path).ok()?;
            let mut parsed_text = parse_formatted_text(message_document.caption());
            parsed_text.push_str(format!("\n\n![]({})", doc.file_name()).as_str());
            return Some(parsed_text);
        }
        MessageContent::MessagePhoto(photo) => {
            log::info!("downloading photo");
            let file = match client_meta
                .client
                .download_file(
                    DownloadFile::builder()
                        // TODO: choose a particular image size: https://core.telegram.org/api/files#image-thumbnail-types
                        .file_id(photo.photo().sizes().first().unwrap().photo().id())
                        .synchronous(true)
                        .priority(1)
                        .build(),
                )
                .await
            {
                Ok(file) => file,
                Err(err) => {
                    log::error!("cannot download file: {}", err);
                    return None;
                }
            };
            let file_name = path::Path::new(file.local().path()).file_name().unwrap();
            let path = path::Path::new(client_meta.file_path.as_str())
                .parent()
                .unwrap()
                .join(&file_name);
            fs::rename(file.local().path(), &path).ok()?;
            log::debug!("downloaded photo to {:?}", path.to_str());
            let mut parsed_text = format!("\n\n![]({})\n\n", file_name.to_str().unwrap());
            parsed_text.push_str(parse_formatted_text(photo.caption()).as_str());
            return Some(parsed_text);
        }
        MessageContent::MessageVideo(message_video) => {
            return Some(parse_formatted_text(message_video.caption()));
        }

        // probably needs to be supported
        MessageContent::MessageLocation(_) => {}
        MessageContent::MessageVideoNote(_) => {}
        MessageContent::MessageVoiceNote(_) => {}

        MessageContent::_Default => {}
        MessageContent::MessageAnimatedEmoji(_) => {}
        MessageContent::MessageBasicGroupChatCreate(_) => {}
        MessageContent::MessageCall(_) => {}
        MessageContent::MessageChatAddMembers(_) => {}
        MessageContent::MessageChatChangePhoto(_) => {}
        MessageContent::MessageChatChangeTitle(_) => {}
        MessageContent::MessageChatDeleteMember(_) => {}
        MessageContent::MessageChatDeletePhoto(_) => {}
        MessageContent::MessageChatJoinByLink(_) => {}
        MessageContent::MessageChatJoinByRequest(_) => {}
        MessageContent::MessageChatSetTheme(_) => {}
        MessageContent::MessageChatUpgradeFrom(_) => {}
        MessageContent::MessageChatUpgradeTo(_) => {}
        MessageContent::MessageContact(_) => {}
        MessageContent::MessageContactRegistered(_) => {}
        MessageContent::MessageCustomServiceAction(_) => {}
        MessageContent::MessageDice(_) => {}
        MessageContent::MessageExpiredPhoto(_) => {}
        MessageContent::MessageExpiredVideo(_) => {}
        MessageContent::MessageGame(_) => {}
        MessageContent::MessageGameScore(_) => {}
        MessageContent::MessageInviteVideoChatParticipants(_) => {}
        MessageContent::MessageInvoice(_) => {}
        MessageContent::MessagePassportDataReceived(_) => {}
        MessageContent::MessagePassportDataSent(_) => {}
        MessageContent::MessagePaymentSuccessful(_) => {}
        MessageContent::MessagePaymentSuccessfulBot(_) => {}
        MessageContent::MessagePinMessage(_) => {}
        MessageContent::MessagePoll(_) => {}
        MessageContent::MessageProximityAlertTriggered(_) => {}
        MessageContent::MessageScreenshotTaken(_) => {}
        MessageContent::MessageSticker(_) => {}
        MessageContent::MessageSupergroupChatCreate(_) => {}
        MessageContent::MessageUnsupported(_) => {}
        MessageContent::MessageVenue(_) => {}
        MessageContent::MessageVideoChatEnded(_) => {}
        MessageContent::MessageVideoChatScheduled(_) => {}
        MessageContent::MessageVideoChatStarted(_) => {}
        MessageContent::MessageBotWriteAccessAllowed(_) => {}
        MessageContent::MessageChatSetBackground(_) => {}
        MessageContent::MessageChatSetMessageAutoDeleteTime(_) => {}
        MessageContent::MessageChatShared(_) => {}
        MessageContent::MessageForumTopicCreated(_) => {}
        MessageContent::MessageForumTopicEdited(_) => {}
        MessageContent::MessageForumTopicIsClosedToggled(_) => {}
        MessageContent::MessageForumTopicIsHiddenToggled(_) => {}
        MessageContent::MessageGiftedPremium(_) => {}
        MessageContent::MessagePremiumGiftCode(_) => {}
        MessageContent::MessagePremiumGiveaway(_) => {}
        MessageContent::MessagePremiumGiveawayCreated(_) => {}
        MessageContent::MessageStory(_) => {}
        MessageContent::MessageSuggestProfilePhoto(_) => {}
        MessageContent::MessageUserShared(_) => {}
        MessageContent::MessageWebAppDataReceived(_) => {}
        MessageContent::MessageWebAppDataSent(_) => {}
    }
    None
}

fn parse_formatted_text(formatted_text: &FormattedText) -> String {
    let mut entities_by_index = make_entities_stack(formatted_text.entities());
    let mut result_text = String::new();
    let mut current_entity = match entities_by_index.pop() {
        None => return formatted_text.text().clone(),
        Some(entity) => entity,
    };
    for (i, ch) in formatted_text.text().encode_utf16().enumerate() {
        let c = char::from_u32(ch as u32).unwrap_or(' ').to_string();
        if i == current_entity.0 {
            result_text.push_str(current_entity.1.as_str());
            current_entity = loop {
                match entities_by_index.pop() {
                    None => {
                        result_text = format!(
                            "{}{}",
                            result_text,
                            &formatted_text
                                .text()
                                .chars()
                                .skip(i + 1)
                                .take(formatted_text.text().len() - i)
                                .collect::<String>()
                        );
                        return result_text;
                    }
                    Some(entity) => {
                        if entity.0 == i {
                            result_text.push_str(&entity.1);
                        } else {
                            result_text.push_str(c.as_str());
                            break entity;
                        }
                    }
                }
            }
        } else {
            result_text.push_str(c.as_str());
        }
    }

    result_text.push_str(&current_entity.1);

    loop {
        match entities_by_index.pop() {
            None => return result_text,
            Some(entity) => {
                result_text.push_str(&entity.1);
            }
        }
    }
}

fn make_entities_stack(entities: &[TextEntity]) -> Vec<(usize, String)> {
    let mut stack = Vec::new();
    for entity in entities {
        let formatting = match entity.type_() {
            TextEntityType::Bold(_) => Some(("**".to_string(), "**".to_string())),
            TextEntityType::Code(_) => Some(("`".to_string(), "`".to_string())),
            TextEntityType::Hashtag(_) => Some(("#".to_string(), "".to_string())),
            TextEntityType::Italic(_) => Some(("<i>".to_string(), "</i>".to_string())),
            TextEntityType::PhoneNumber(_) => Some(("<phone>".to_string(), "</phone>".to_string())),
            TextEntityType::Pre(_) => Some(("```\n".to_string(), "\n```".to_string())),
            TextEntityType::PreCode(_) => {
                Some(("<pre><code>".to_string(), "</code></pre>".to_string()))
            }
            TextEntityType::Strikethrough(_) => Some(("~~".to_string(), "~~".to_string())),
            TextEntityType::TextUrl(u) => {
                Some(("[".to_string(), format!("]({})", u.url()).to_string()))
            }
            TextEntityType::Underline(_) => Some(("<u>".to_string(), "</u>".to_string())),
            TextEntityType::Url(_) => Some(("<a>".to_string(), "</a>".to_string())),
            TextEntityType::_Default => None,
            // TextEntityType::BankCardNumber(_) => None,
            TextEntityType::BotCommand(_) => None,
            TextEntityType::Cashtag(_) => None,
            TextEntityType::EmailAddress(_) => None,
            TextEntityType::Mention(_) => None,
            TextEntityType::MentionName(_) => None,
            TextEntityType::BankCardNumber(_) => None,
            TextEntityType::MediaTimestamp(_) => None,
            TextEntityType::BlockQuote(_) => None,
            TextEntityType::CustomEmoji(_) => None,
            TextEntityType::Spoiler(_) => None,
        };
        if let Some((start_tag, end_tag)) = formatting {
            stack.push((entity.offset() as usize, start_tag));
            stack.push(((entity.offset() + entity.length()) as usize, end_tag));
        }
    }
    stack.sort_by_key(|(i, _)| *i);
    stack.reverse();
    stack
}

#[cfg(test)]
mod tests {
    use rust_tdlib::types::FormattedText;

    use crate::parse_formatted_text;

    #[test]
    fn test_parse_formatted_text() {
        let tests = vec![
            (
                r#"{"@type": "formattedText", "text": "\uD83D\uDCB8 Налоги в Италии\n\nМы почти 3 месяца рожали этот гайд. Писали, потом переделывали заново. Брали консультации, редактировали снова и в итоге готовы отдать вам текущую обзорную версию основных налогов в Италии. Он не идеален, но уже пора выпустить и двинуться дальше.\n\nВ планах сделать еще несколько детальных гайдов. Более практичных и специализированных. Благо у нас появился человек, который активно занимается этим.\n\n\uD83D\uDD17 Гайд по налогам\n\nГайд написал совместно с нами Александр. Если у вас есть вопросы или желание что-то добавить, сотрудничать в этой области напишите ему.\n\nЖдём ваших предложений и замечаний, наша цель составить самый понятный и детальный гайд по налогам.\n\n\uD83D\uDCAC Обсудить можно в чате", "entities": [{"@type": "textEntity", "offset": 3, "length": 17, "type": {"@type": "textEntityTypeBold"}}, {"@type": "textEntity", "offset": 423, "length": 15, "type": {"@type": "textEntityTypeTextUrl", "url": "https://rutoitaly.ru/wiki/Imposte_e_tasse"}}, {"@type": "textEntity", "offset": 423, "length": 15, "type": {"@type": "textEntityTypeBold"}}, {"@type": "textEntity", "offset": 470, "length": 10, "type": {"@type": "textEntityTypeTextUrl", "url": "https://t.me/alx4039"}}, {"@type": "textEntity", "offset": 577, "length": 101, "type": {"@type": "textEntityTypeItalic"}}, {"@type": "textEntity", "offset": 678, "length": 2, "type": {"@type": "textEntityTypeCustomEmoji", "custom_emoji_id": "5443038326535759644"}}, {"@type": "textEntity", "offset": 681, "length": 21, "type": {"@type": "textEntityTypeTextUrl", "url": "https://t.me/rutoitalychat/13295/22683"}}, {"@type": "textEntity", "offset": 681, "length": 21, "type": {"@type": "textEntityTypeItalic"}}]}"#,
                "   <b>Налоги в Италии\n\n</b>Мы почти 3 месяца рожали этот гайд. Писали, потом переделывали заново. Брали консультации, редактировали снова и в итоге готовы отдать вам текущую обзорную версию основных налогов в Италии. Он не идеален, но уже пора выпустить и двинуться дальше.\n\nВ планах сделать еще несколько детальных гайдов. Более практичных и специализированных. Благо у нас появился человек, который активно занимается этим.\n\n   <a href=\"https://rutoitaly.ru/wiki/Imposte_e_tasse\"><b>Гайд по налогам</a></b>\n\nГайд написал совместно с нами <a href=\"https://t.me/alx4039\">Александр.</a> Если у вас есть вопросы или желание что-то добавить, сотрудничать в этой области напишите ему.\n\n<i>Ждём ваших предложений и замечаний, наша цель составить самый понятный и детальный гайд по налогам.\n\n</i>   <a href=\"https://t.me/rutoitalychat/13295/22683\"><i>Обсудить можно в чате</a></i>"
            ),
            (
                r#"{"@type":"formattedText","@extra":"","text":"Изображение из пятидесяти линий.\nНаткнулся на скрипт, который генерирует такие изображения вот тут.\nЛожите рядом со скриптом png изображение 750х750 в градациях серого, в исходнике меняете имя файла на ваше и запускаете исходник с помощью processing. Сгенерированное изображение будет лежать в том же каталоге.","entities":[{"@type":"textEntity","@extra":"","offset":91,"length":7,"type":{"@type":"textEntityTypeTextUrl","@extra":"","url":"https://gist.github.com/u-ndefine/8e4bc21be4275f87fefe7b2a68487161"}},{"@type":"textEntity","@extra":"","offset":239,"length":10,"type":{"@type":"textEntityTypeTextUrl","@extra":"","url":"https://processing.org/download/"}}]}"#,
                r#"Изображение из пятидесяти линий.
Наткнулся на скрипт, который генерирует такие изображения <a href="https://gist.github.com/u-ndefine/8e4bc21be4275f87fefe7b2a68487161">вот тут</a>.
Ложите рядом со скриптом png изображение 750х750 в градациях серого, в исходнике меняете имя файла на ваше и запускаете исходник с помощью <a href="https://processing.org/download/">processing</a> Сгенерированное изображение будет лежать в том же каталоге."#,
            ),
            (
                r#"{"@type":"formattedText","@extra":"","text":"Напоминаем, что здесь у нас есть ещё и свой чат, где проходят «публичные» интервью, а в свободное время можно просто потрещать за жизнь\n\nЗаходи, тебе здесь рады)\n\nhttps://t.me/joinchat/IqlQqUGyZpI1-0Zu8ChAmA","entities":[]}"#,
                r#"Напоминаем, что здесь у нас есть ещё и свой чат, где проходят «публичные» интервью, а в свободное время можно просто потрещать за жизнь

Заходи, тебе здесь рады)

https://t.me/joinchat/IqlQqUGyZpI1-0Zu8ChAmA"#,
            ),
        ];
        for (json_data, expected) in tests {
            let formatted_text = FormattedText::from_json(json_data).expect("cannot parse json");
            let t = parse_formatted_text(&formatted_text);
            assert_eq!(t, expected);
        }
    }
}
