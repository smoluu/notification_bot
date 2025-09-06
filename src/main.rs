use std::collections::{ HashMap, HashSet };
use std::fmt::format;
use std::io::Write;
use std::path::PathBuf;
use std::process::exit;
use std::sync::Arc;
use std::time::{ Duration, Instant };
use std::fs::{ read_to_string, OpenOptions };
use dotenv::dotenv;
use log::{ debug, error, info };
use teloxide::dispatching::dialogue::{ InMemStorage, Dialogue };
use tokio::fs;
use tokio::sync::{ Mutex, oneshot };
use tokio::process::Command;
use teloxide::{ prelude::*, types::ChatId, RequestError, Bot };
use tokio::time::{ sleep };
use serde::{ Serialize, Deserialize };

#[derive(Debug, Deserialize, Serialize, Clone)]
struct BotConfig {
    ping_interval: u64,
}
impl Default for BotConfig {
    fn default() -> Self {
        BotConfig { ping_interval: 60 }
    }
}

#[derive(Default)]
struct AppState {
    allowed_chats: Vec<ChatId>,
    hosts_path: PathBuf,
    hosts: HashMap<String, bool>,
    password: String,
}
#[derive(Default, Debug)]
struct BotState {
    task: Option<oneshot::Sender<()>>,
    chat_id: Option<ChatId>,
    config: BotConfig,
}

#[derive(Clone, Serialize, Deserialize, Default)]
enum DialogueState {
    #[default]
    Default,
    WaitingForPassword,
    WaitingForHostAdd,
    WaitingForHostRemove,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    pretty_env_logger::init();

    dotenv().ok();
    let mut hosts_path = PathBuf::new();

    if cfg!(not(debug_assertions)) {
        hosts_path.push("/etc/notification_bot/hosts.txt");
    } else {
        hosts_path.push("hosts.txt");
    }

    let bot = Bot::from_env();
    let bot_state = Arc::new(Mutex::new(BotState::default()));
    let app_state = Arc::new(
        Mutex::new(AppState {
            password: std::env::var("BOT_PASSWORD").unwrap_or("default_password".to_string()),
            hosts_path: hosts_path,
            ..Default::default()
        })
    );
    // read and load config
    let bot_config_path = "config.toml";
    let result = match fs::read_to_string(&bot_config_path).await {
        Ok(r) => r,
        Err(_) => {
            error!("Could not read bot configuration file");
            exit(1);
        }
    };
    match toml::from_str(&result) {
        Ok(result) => {
            let mut bot_state_guard = bot_state.lock().await;
            bot_state_guard.config = result;
        }
        Err(e) => {
            error!("Unable to load data from {} => {}", bot_config_path, e);
            exit(1);
        }
    }
    debug!("bot state, {:?}", bot_state);

    let bot_state_clone = Arc::clone(&bot_state);
    let app_state_clone = Arc::clone(&app_state);

    let dialogue_storage = InMemStorage::<DialogueState>::new();

    let mut app_state_guard = app_state.lock().await;
    app_state_guard.hosts = read_to_string(app_state_guard.hosts_path.clone())
        .unwrap()
        .lines()
        .map(String::from)
        .collect::<HashSet<_>>()
        .into_iter()
        .map(|host| (host, true))
        .collect();
    info!("HOSTS -> {:?}", app_state_guard.hosts);
    drop(app_state_guard);

    let handler = Update::filter_message()
        .enter_dialogue::<Message, InMemStorage<DialogueState>, DialogueState>()
        .endpoint(dialogue_handler);

    Dispatcher::builder(bot, handler)
        .dependencies(dptree::deps![bot_state_clone, app_state_clone, dialogue_storage])
        .default_handler(|_| async move { () })
        .build()
        .dispatch().await;

    Ok(())
}

async fn dialogue_handler(
    bot: Bot,
    msg: Message,
    dialogue: Dialogue<DialogueState, InMemStorage<DialogueState>>,
    bot_state: Arc<Mutex<BotState>>,
    app_state: Arc<Mutex<AppState>>
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
                // start timer for host scan
                let scan_start = Instant::now();

                for (ip, _) in hosts {
                    let handle = tokio::spawn(async move {
                        let output = Command::new("/bin/nmap")
                            .args(["-T3", "-sT", "-Pn", "--host-timeout", "10", ip.as_str()])
                            .output().await;
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
                            Err(e) =>
                                (false, format!("PING FAILED TO HOST -> {}, error -> {}", ip, e)),
                        }
                    });
                    handles.push(handle);
                }

                let mut responses: Vec<String> = Vec::new();
                for handle in handles {
                    match handle.await {
                        Ok(result) => {
                            // remove empty lines from each result
                            let result = result.1
                                .lines()
                                .filter(|line| !line.trim().is_empty())
                                .collect::<Vec<&str>>()
                                .join("\n");
                            responses.push(result);
                        }
                        Err(e) => info!("ERROR -> {}", e),
                    }
                }
                let scan_time = scan_start.elapsed().as_secs_f64();

                // combine results to one string and remove unneccesary text
                let mut combined_string = responses
                    .iter()
                    .map(|output| {
                        // split output into lines, skip the first line, and join with newlines
                        output.lines().skip(1).collect::<Vec<_>>().join("\n") + "\n\n" // add newlines to separate results
                    })
                    .collect::<String>();
                info!("{}", combined_string);

                combined_string += format!(
                    "Nmap scan finnished in {scan_time:.2} seconds"
                ).as_str();

                bot.send_message(chat_id, &combined_string).await?;
            } else if
                // /start command
                text.starts_with("/start")
            {
                let mut bot_state_guard = bot_state.lock().await;

                if bot_state_guard.task.is_some() {
                    bot.send_message(chat_id, "Task is already running!").await?;
                    return Ok(());
                }

                bot_state_guard.chat_id = Some(chat_id);
                info!("Host monitoring task started. \nChat ID: {}", chat_id);

                let (tx, rx) = oneshot::channel();
                bot_state_guard.task = Some(tx);
                let bot_config = bot_state_guard.config.clone();
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
                            _ = sleep(Duration::from_secs(bot_config.ping_interval)) => {
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
                    format!("Notification Bot started. Your chat ID is: {}", chat_id)
                ).await?;
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
            } else if text.starts_with("/add") {
                bot.send_message(chat_id, "Enter hostname you want to add.").await?;

                if let Err(e) = dialogue.update(DialogueState::WaitingForHostAdd).await {
                    info!("Dialogue update error: {}", e);
                }
                return Ok(());
            } else if text.starts_with("/remove") {
                let hosts = {
                    let app_state_guard = app_state.lock().await;
                    app_state_guard.hosts.clone()
                };
                let hosts_string = hosts
                    .iter()
                    .map(|(host, _)| format!("{}", host))
                    .collect::<Vec<_>>()
                    .join("\n");
                bot.send_message(
                    chat_id,
                    format!("Enter hostname you want to remove.\n{}", hosts_string)
                ).await?;
                if let Err(e) = dialogue.update(DialogueState::WaitingForHostRemove).await {
                    info!("Dialogue update error: {}", e);
                }

                return Ok(());
            } else if text.starts_with("/hosts") {
                let hosts = {
                    let app_state_guard = app_state.lock().await;
                    app_state_guard.hosts.clone()
                };

                let hosts_string = hosts
                    .iter()
                    .enumerate()
                    .map(|(index, (host, _))| format!(" {}: {}", index + 1, host))
                    .collect::<Vec<_>>()
                    .join("\n");

                bot.send_message(chat_id, format!("Hosts: \n {}", hosts_string)).await?;
                info!("Listed hosts \n{} ", hosts_string);

                return Ok(());
            } else if text.starts_with("/config") {
                let input = text.to_ascii_lowercase();
                let args: Vec<&str> = input.split(" ").collect();
                if args.len() > 1 {
                    match args[1] {
                        "edit" => {
                            if let Some(_) = args.get(2..4) {
                                let mut bot_state_guard = bot_state.lock().await;
                                let field = args[2];
                                let value = args[3];
                                match field {
                                    "ping_interval" => {
                                        match value.parse::<u64>() {
                                            Ok(value) => {
                                                bot_state_guard.config.ping_interval = value;
                                                bot.send_message(
                                                    chat_id,
                                                    format!("Ping interval changed to {}", value)
                                                ).await?;
                                            }
                                            Err(e) => {
                                                bot.send_message(
                                                    chat_id,
                                                    format!("Invalid value: {}", e)
                                                ).await?;
                                            }
                                        }
                                    }
                                    _ => {
                                        bot.send_message(chat_id, "Invalid arguments").await?;
                                    }
                                }
                                // write new config to file
                                let toml_config = toml::to_string(&bot_state_guard.config).unwrap();
                                fs::write("config.toml", toml_config).await.unwrap();

                                debug!("edit_args: {:?}", args);
                            } else {
                                bot.send_message(chat_id, "Not enought arguments").await?;
                            }
                        }
                        "list" => {
                            let bot_config = {
                                let bot_state_guard = bot_state.lock().await;
                                bot_state_guard.config.clone()
                            };
                            bot.send_message(chat_id, format!("{:?}", bot_config)).await?;
                        }
                        _ => {
                            bot.send_message(chat_id, "Invalid input").await?;
                        }
                    }
                } else {
                    bot.send_message(
                        chat_id,
                        "/config list     - Show current config \n /config edit <field> <value>     - Update config field"
                    ).await?;
                }

                return Ok(());
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
                    "Password accepted! You can now use /start, /stop, /status, /hosts, /add, /remove."
                ).await?;
                if let Err(e) = dialogue.update(DialogueState::Default).await {
                    info!("Dialogue update error: {}", e);
                }
            } else {
                bot.send_message(chat_id, "Incorrect password. Try again.").await?;
            }
        }

        DialogueState::WaitingForHostAdd => {
            let mut new_host = "\n".to_string();
            new_host.push_str(text);

            let mut app_state_guard = app_state.lock().await;
            // add new host to hosts file
            let mut paths_file = OpenOptions::new()
                .append(true)
                .open(app_state_guard.hosts_path.clone())
                .expect("cannot open file");

            paths_file.write(new_host.as_bytes()).expect("Write failed to hosts.txt");

            // set app_sate.hosts with updated hosts file
            app_state_guard.hosts = read_to_string(app_state_guard.hosts_path.clone())
                .unwrap()
                .lines()
                .map(String::from)
                .collect::<HashSet<_>>()
                .into_iter()
                .map(|host| (host, true))
                .collect();
            info!("New hosts for {} -> {:?}", chat_id, app_state_guard.hosts);

            bot.send_message(chat_id, "New host added.").await?;
            info!("Added {} from hosts", new_host);

            if let Err(e) = dialogue.update(DialogueState::Default).await {
                info!("Dialogue update error: {}", e);
            }
        }

        DialogueState::WaitingForHostRemove => {
            let host_remove = text;
            let mut app_state_guard = app_state.lock().await;

            // remove hosts from app_state.hosts
            if app_state_guard.hosts.remove(host_remove).is_none() {
                bot.send_message(chat_id, format!("Host '{}' not found.", host_remove)).await?;
                if let Err(e) = dialogue.update(DialogueState::Default).await {
                    info!("Dialogue update error: {}", e);
                }
                return Ok(());
            }

            // generate updated hosts file string
            let hosts: Vec<&str> = app_state_guard.hosts
                .keys()
                .map(|host| host.as_str()) // Convert &String to &str
                .collect();
            let updated_hosts = hosts.join("\n");

            // write new hosts file
            let mut hosts_file = OpenOptions::new()
                .write(true)
                .truncate(true)
                .open(&app_state_guard.hosts_path)
                .expect("Cant open file");
            hosts_file
                .write_all(updated_hosts.as_bytes())
                .expect("Cant open hosts.txt for writing");
            bot.send_message(chat_id, format!("Host '{}' removed.", host_remove)).await?;
            info!("Removed {} from hosts", host_remove);

            if let Err(e) = dialogue.update(DialogueState::Default).await {
                info!("Dialogue update error: {}", e);
            }
        }
    }

    Ok(())
}
