use std::collections::HashMap;
use std::sync::Arc;
use std::fs::read_to_string;
use dotenv::dotenv;
use log::{ debug, info };
use tokio::sync::{ Mutex, oneshot };
use tokio::process::Command;
use teloxide::{ prelude::*, types::ChatId, RequestError, Bot };
use tokio::time::{ sleep, Duration };

const PING_INTERVAL: u64 = 60; // in seconds
const HOST_FILE: &str = "hosts.txt";
const HOST_LIST: [&str; 6] = [
    "192.168.69.200",
    "192.168.69.201",
    "192.168.69.11",
    "192.168.69.12",
    "192.168.69.143",
    "192.168.69.2",
];

// state to track the task and ChatId
#[derive(Default)]
struct BotState {
    task: Option<oneshot::Sender<()>>, // to signal task termination
    chat_id: Option<ChatId>,
    hosts: HashMap<String, bool>, // hostname, isonline
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    pretty_env_logger::init();
    dotenv().ok();

    let bot = Bot::from_env();
    // Shared state and ChatId for tasks
    let state = Arc::new(Mutex::new(BotState::default()));
    let state_clone = Arc::clone(&state);

    // load hosts.txt to a vector
    let mut hosts: Vec<String> = Vec::new();
    for line in read_to_string(HOST_FILE).unwrap().lines() {
        hosts.push(line.to_string());
    }
    info!("HOSTS -> {:?}", hosts);

    // insert hosts to bot state
    let _ = {
        let mut lock = state.lock().await;
        for host in hosts {
            lock.hosts.insert(host, true);
        }

        info!("HOSTS -> {:?}", lock.hosts);
    };

    // Set up command handler
    let handler = Update::filter_message().endpoint(
        |bot: Bot, msg: Message, state: Arc<Mutex<BotState>>| async move {
            let chat_id = msg.chat.id;
            let text = msg.text().unwrap_or("");

            if text.starts_with("/status") {
                let mut handles = Vec::new();
                let hosts = {
                    let state_guard = state.lock().await;
                    state_guard.hosts.clone()
                };
                for (k, _) in hosts {
                    let handle = tokio::spawn(async move {
                        let output = Command::new("ping")
                            .args(["-l", "1", "-c", "3", "-W", "0.5", k.as_str()])
                            .output().await;
                        match output {
                            Ok(output) => {
                                let stdout = String::from_utf8_lossy(&output.stdout);
                                let stderr = String::from_utf8_lossy(&output.stderr);
                                if output.status.success() {
                                    (output.status.success(), stdout.to_string())
                                } else {
                                    (output.status.success(), stderr.to_string())
                                }
                            }
                            Err(e) => {
                                log::error!("PING ERROR => {}", e);
                                (
                                    false,
                                    format!(
                                        "PING FAILED TO HOST -> {}, error ->  {}",
                                        k.as_str(),
                                        e
                                    ),
                                )
                            }
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
                    .fold(String::new(), |acc, (_, s)| acc + s + "\n");
                bot.send_message(chat_id, &combined_string).await?;
            }

            if text.starts_with("/start") {
                debug!("start");
                let mut state_guard = state.lock().await;
                if state_guard.task.is_some() {
                    bot.send_message(chat_id, "Task is already running!").await?;
                    return Ok::<(), RequestError>(()); // Explicit error type
                }
                debug!("start");

                // store ChatId
                state_guard.chat_id = Some(chat_id);
                log::info!(
                    "Host monitoring task started. \nChat ID: {}\n Monitored hosts {:?}",
                    chat_id,
                    HOST_LIST
                );

                // create a oneshot channel to signal task termination
                let (tx, rx) = oneshot::channel();
                state_guard.task = Some(tx);
                drop(state_guard); // Release the lock

                // clone bot for the task
                let bot_clone = bot.clone();
                let state_clone = Arc::clone(&state);

                // spawn the periodic task
                tokio::spawn(async move {
                    let mut rx = rx; // Move receiver into task
                    loop {
                        tokio::select! {
                            _ = &mut rx => {
                                log::info!("Task for Chat ID {} stopped", chat_id);
                                break;
                            }
                            _ = sleep(Duration::from_secs(PING_INTERVAL)) => {

                                let hosts = {
                                    let state_guard = state.lock().await;
                                    state_guard.hosts.clone()
                                };
                                for (adress, online) in hosts {
                                    if online {
                                        // ping -l preload 3 packets,-c ping count -W timeout 0.5 seconds 
                                        let output = Command::new("ping").args(["-l", "1", "-c", "3", "-W", "0.5", adress.as_str()]).output().await;
                                        match output {
                                            Ok(output) => {
                                                let stdout = String::from_utf8_lossy(&output.stdout);
                                                //let stderr = String::from_utf8_lossy(&output.stderr);
                                                if !output.status.success() {
                                                    let mut state_guard = state_clone.lock().await;
                                                    state_guard.hosts.insert(adress, false);
                                                    let _ = send_message(&bot_clone, chat_id, &format!("HOST OFFLINE -> STDOUT {}", &stdout)).await;
                                                };
                                            }
                                            Err(e) => log::error!("PING ERROR => {}", e)
                                        }
                                    }
                                    
                                }
                             
                            }
                        }
                    }
                    // update state to clear task after termination
                    let mut state_guard = state_clone.lock().await;
                    state_guard.task = None;
                });

                bot.send_message(
                    chat_id,
                    format!("Notification Bot started. Your chat ID is: {}", chat_id)
                ).await?;
            } else if text.starts_with("/stop") {
                let mut state_guard = state.lock().await;
                if let Some(tx) = state_guard.task.take() {
                    // Signal the task to stop
                    if tx.send(()).is_ok() {
                        bot.send_message(chat_id, "Task stopped.").await?;
                        log::info!("Task stopped for Chat ID: {}", chat_id);
                    } else {
                        bot.send_message(chat_id, "Failed to stop task.").await?;
                    }
                } else {
                    bot.send_message(chat_id, "No task is running.").await?;
                }
            }

            Ok::<(), RequestError>(()) // Explicit error type
        }
    );

    // Start the dispatcher
    Dispatcher::builder(bot, handler)
        .dependencies(dptree::deps![state_clone])
        .build()
        .dispatch().await;

    Ok(())
}

async fn send_message(bot: &Bot, chat_id: ChatId, message: &str) -> Result<(), RequestError> {
    log::info!("Sending message to Chat ID: {}", chat_id);
    bot.send_message(chat_id, message).await?;
    Ok(())
}
