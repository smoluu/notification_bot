use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::fs::read_to_string;
use dotenv::dotenv;
use log::info;
use teloxide::dispatching::dialogue::{InMemStorage, Dialogue, InMemStorageError};
use tokio::sync::{Mutex, oneshot};
use tokio::process::Command;
use teloxide::{prelude::*, types::ChatId, RequestError, Bot};
use tokio::time::{sleep, Duration};
use serde::{Serialize, Deserialize};

const PING_INTERVAL: u64 = 60;

#[derive(Default)]
struct AppState {
    allowed_chats: Vec<ChatId>,
    hosts: HashMap<String, bool>,
    password: String,
}

#[derive(Default)]
struct BotState {
    task: Option<oneshot::Sender<()>>,
    chat_id: Option<ChatId>,
}

#[derive(Clone, Serialize, Deserialize, Default)]
enum DialogueState {
    #[default]
    Default,
    WaitingForPassword,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    pretty_env_logger::init();
    dotenv().ok();
    let mut hosts_file = PathBuf::new();

    if cfg!(not(debug_assertions)) {
        hosts_file.push("/etc/notification_bot/hosts.txt");
    } else {
        hosts_file.push("hosts.txt");
    }

    let bot = Bot::from_env();
    let bot_state = Arc::new(Mutex::new(BotState::default()));
    let app_state = Arc::new(Mutex::new(AppState {
        password: std::env::var("BOT_PASSWORD").unwrap_or("default_password".to_string()),
        ..Default::default()
    }));
    let bot_state_clone = Arc::clone(&bot_state);
    let app_state_clone = Arc::clone(&app_state);

    let dialogue_storage = InMemStorage::<DialogueState>::new();

    let mut hosts: Vec<String> = Vec::new();
    for line in read_to_string(hosts_file).unwrap().lines() {
        hosts.push(line.to_string());
    }

    {
        let mut app_state_guard = app_state.lock().await;
        for host in hosts {
            app_state_guard.hosts.insert(host, true);
        }
        info!("HOSTS -> {:?}", app_state_guard.hosts);
    }

    let handler = Update::filter_message()
        .enter_dialogue::<Message, InMemStorage<DialogueState>, DialogueState>()
        .endpoint(dialogue_handler);

    Dispatcher::builder(bot, handler)
        .dependencies(dptree::deps![bot_state_clone, app_state_clone, dialogue_storage])
        .default_handler(|_| async move { () })
        .build()
        .dispatch()
        .await;

    Ok(())
}

async fn dialogue_handler(
    bot: Bot,
    msg: Message,
    dialogue: Dialogue<DialogueState, InMemStorage<DialogueState>>,
    bot_state: Arc<Mutex<BotState>>,
    app_state: Arc<Mutex<AppState>>,
) -> Result<(), RequestError> {
    let chat_id = msg.chat.id;
    let text = msg.text().unwrap_or("");
    let state = match dialogue.get().await {
        Ok(state) => state.unwrap_or(DialogueState::Default),
        Err(e) => {
            info!("Dialogue error: {}", e);
            DialogueState::Default
        }
    };

    match state {
        DialogueState::Default => {
            let allowed_chats = {
                let app_state_guard = app_state.lock().await;
                app_state_guard.allowed_chats.clone()
            };

            if !allowed_chats.contains(&chat_id) {
                bot.send_message(chat_id, "Enter password").await?;
                if let Err(e) = dialogue.update(DialogueState::WaitingForPassword).await {
                    info!("Dialogue update error: {}", e);
                }
                return Ok(());
            }

            if text.starts_with("/status") {
                let mut handles = Vec::new();
                let hosts = {
                    let app_state_guard = app_state.lock().await;
                    app_state_guard.hosts.clone()
                };
                for (ip, _) in hosts {
                    let handle = tokio::spawn(async move {
                        let output = Command::new("/bin/nmap")
                            .args(["-T5", "-sT","--host-timeout", "5000", ip.as_str()])
                            .output()
                            .await;
                        match output {
                            Ok(output) => {
                                let stdout = String::from_utf8_lossy(&output.stdout).to_string();
                                if output.status.success() {
                                    (true, format!("Host {}: {}", ip, stdout))
                                } else {
                                    let stderr = String::from_utf8_lossy(&output.stderr);
                                    (false, format!("Host {} failed: {}", ip, stderr))
                                }
                            }
                            Err(e) => (
                                false,
                                format!("PING FAILED TO HOST -> {}, error -> {}", ip, e),
                            ),
                        }
                    });
                    handles.push(handle);
                }
                let mut responses: Vec<(bool, String)> = Vec::new();
                for handle in handles {
                    match handle.await {
                        Ok(result) => responses.push(result),
                        Err(e) => info!("ERROR -> {}", e),
                    }
                }
                let combined_string = responses
                    .iter()
                    .map(|(_success, s)| format!("{}\n", s))
                    .collect::<Vec<_>>()
                    .join("");
                info!("{}", combined_string);
                bot.send_message(chat_id, &combined_string).await?;
            } else if text.starts_with("/start") {
                let mut bot_state_guard = bot_state.lock().await;

                if bot_state_guard.task.is_some() {
                    bot.send_message(chat_id, "Task is already running!").await?;
                    return Ok(());
                }

                bot_state_guard.chat_id = Some(chat_id);
                info!("Host monitoring task started. \nChat ID: {}", chat_id);

                let (tx, rx) = oneshot::channel();
                bot_state_guard.task = Some(tx);
                let bot_clone = bot.clone();
                let app_state_clone = Arc::clone(&app_state);
                let bot_state_clone = Arc::clone(&bot_state);

                tokio::spawn(async move {
                    let mut rx = rx;
                    loop {
                        tokio::select! {
                            _ = &mut rx => {
                                info!("Task for Chat ID {} stopped", chat_id);
                                break;
                            }
                            _ = sleep(Duration::from_secs(PING_INTERVAL)) => {
                                let hosts = {
                                    let app_state_guard = app_state_clone.lock().await;
                                    app_state_guard.hosts.clone()
                                };
                                for (address, online) in hosts {
                                    if online {
                                        let output = Command::new("ping")
                                            .args(["-l", "1", "-c", "3", "-W", "0.5", address.as_str()])
                                            .output()
                                            .await;
                                        match output {
                                            Ok(output) => {
                                                let stdout = String::from_utf8_lossy(&output.stdout);
                                                if !output.status.success() {
                                                    let mut app_state_guard = app_state_clone.lock().await;
                                                    app_state_guard.hosts.insert(address, false);
                                                    let _ = bot_clone
                                                        .send_message(chat_id, &format!("HOST OFFLINE -> STDOUT {}", &stdout))
                                                        .await;
                                                }
                                            }
                                            Err(e) => info!("PING ERROR => {}", e),
                                        }
                                    }
                                }
                            }
                        }
                    }
                    let mut bot_state_guard = bot_state_clone.lock().await;
                    bot_state_guard.task = None;
                });

                bot.send_message(
                    chat_id,
                    format!("Notification Bot started. Your chat ID is: {}", chat_id),
                )
                .await?;
            } else if text.starts_with("/stop") {
                let mut bot_state_guard = bot_state.lock().await;
                if let Some(tx) = bot_state_guard.task.take() {
                    if tx.send(()).is_ok() {
                        bot.send_message(chat_id, "Task stopped.").await?;
                        info!("Task stopped for Chat ID: {}", chat_id);
                    } else {
                        bot.send_message(chat_id, "Failed to stop task.").await?;
                    }
                } else {
                    bot.send_message(chat_id, "No task is running.").await?;
                }
            }
        }
        DialogueState::WaitingForPassword => {
            let password = {
                let app_state_guard = app_state.lock().await;
                app_state_guard.password.clone()
            };

            if text == password {
                {
                    let mut app_state_guard = app_state.lock().await;
                    app_state_guard.allowed_chats.push(chat_id);
                }
                bot.send_message(
                    chat_id,
                    "Password accepted! You can now use /start, /stop, or /status.",
                )
                .await?;
                if let Err(e) = dialogue.update(DialogueState::Default).await {
                    info!("Dialogue update error: {}", e);
                }
            } else {
                bot.send_message(chat_id, "Incorrect password. Try again.").await?;
            }
        }
    }

    Ok(())
}