use rust_tdlib::types::{FormattedText, GetMe, MessageContent, TextEntity, TextEntityType};
use rust_tdlib::{
    client::{Client, Worker},
    tdjson,
    types::{SetTdlibParameters, Update},
};
use std::fs::OpenOptions;
use std::io::Write;
use std::{env, fs};
use std::sync::OnceLock;

static CHAT_ID: OnceLock<i64> = OnceLock::new();

#[tokio::main]
async fn main() {
    tdjson::set_log_verbosity_level(
        env::var("TDLIB_LOG_VERBOSITY")
            .unwrap_or_else(|_| "1".to_string())
            .parse()
            .unwrap(),
    );
    env_logger::init();
    let tdlib_parameters = SetTdlibParameters::builder()
        .database_directory("tddb")
        .use_test_dc(false)
        .api_id(env::var("API_ID").unwrap().parse::<i32>().unwrap())
        .api_hash(env::var("API_HASH").unwrap())
        .system_language_code("en")
        .device_model("Desktop")
        .system_version("Unknown")
        .application_version(env!("CARGO_PKG_VERSION"))
        .enable_storage_optimizer(true)
        .build();

    let (sender, mut receiver) = tokio::sync::mpsc::channel::<Box<Update>>(100);
    let client = Client::builder()
        .with_tdlib_parameters(tdlib_parameters)
        .with_updates_sender(sender)
        .build()
        .unwrap();

    let dir_path = env::var("DIR_PATH").unwrap();
    fs::create_dir_all(&dir_path).unwrap();

    tokio::spawn(async move {
        let mut file = OpenOptions::new()
            .write(true)
            .append(true)
            .create(true)
            .open(dir_path + "/telegram.md")
            .unwrap();

        while let Some(message) = receiver.recv().await {
            match message.as_ref() {
                Update::NewMessage(new_message) => {
                    log::info!("New message: {:?}", new_message.message().content());
                    match CHAT_ID.get() {
                        None => {
                            continue;
                        }
                        Some(v) => {
                            if &new_message.message().chat_id() != v {
                                continue;
                            }
                        }
                    }
                    let text = parse_message_content(new_message.message().content());
                    match text {
                        Some(text) => {
                            file.write_all(format!("{}\n", text).as_bytes()).unwrap();
                        }
                        None => {}
                    }
                }
                _ => {}
            }
        }
    });

    // let auth_handler = ClientAuthStateHandler::new(rx);
    let mut worker = Worker::builder().build().unwrap();
    let waiter = worker.start();

    let client = worker.bind_client(client).await.unwrap();
    // tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;
    let me = client.get_me(GetMe::builder().build()).await.unwrap();
    CHAT_ID.get_or_init(|| me.id());
    tokio::time::sleep(tokio::time::Duration::from_secs(60)).await;

    worker.stop();
    let _ = waiter.await;
}

fn parse_message_content(content: &MessageContent) -> Option<String> {
    match content {
        MessageContent::MessageText(text) => return Some(parse_formatted_text(text.text())),
        MessageContent::MessageAnimation(message_animation) => {
            return Some(parse_formatted_text(message_animation.caption()))
        }
        MessageContent::MessageAudio(message_audio) => {
            return Some(parse_formatted_text(message_audio.caption()))
        }
        MessageContent::MessageDocument(message_document) => {
            return Some(parse_formatted_text(message_document.caption()))
        }
        MessageContent::MessagePhoto(photo) => return Some(parse_formatted_text(photo.caption())),
        MessageContent::MessageVideo(message_video) => {
            return Some(parse_formatted_text(message_video.caption()))
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
            TextEntityType::Bold(_) => Some(("<b>".to_string(), "</b>".to_string())),
            TextEntityType::Code(_) => Some(("<code>".to_string(), "</code>".to_string())),
            TextEntityType::Hashtag(_) => Some(("#".to_string(), "".to_string())),
            TextEntityType::Italic(_) => Some(("<i>".to_string(), "</i>".to_string())),
            TextEntityType::PhoneNumber(_) => Some(("<phone>".to_string(), "</phone>".to_string())),
            TextEntityType::Pre(_) => Some(("<pre>".to_string(), "</pre>".to_string())),
            TextEntityType::PreCode(_) => {
                Some(("<pre><code>".to_string(), "</code></pre>".to_string()))
            }
            TextEntityType::Strikethrough(_) => {
                Some(("<strike>".to_string(), "</strike>".to_string()))
            }
            TextEntityType::TextUrl(u) => {
                let tag = format!(r#"<a href="{}">"#, u.url());
                Some((tag, "</a>".to_string()))
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
    use crate::parse_formatted_text;
    use rust_tdlib::types::FormattedText;

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
            let formatted_text = FormattedText::from_json(json_data).unwrap();
            let t = parse_formatted_text(&formatted_text);
            assert_eq!(t, expected);
        }
    }
}
