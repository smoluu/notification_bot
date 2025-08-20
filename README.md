# Notification Bot

A Rust-based Telegram bot for monitoring hosts using `nmap` and `ping` commands. The bot supports starting/stopping host monitoring, checking host status, and requires password authentication for access. It reads a list of hosts from a `hosts.txt` file and sends notifications via Telegram.

## Features
- **Host Monitoring**: Periodically pings hosts listed in `hosts.txt` and notifies if a host goes offline.
- **Status Check**: Runs `nmap` scans on demand to check host status and ports, with filtered output for clarity.
- **Password Protection**: Restricts bot access to authorized Telegram chats via a password.
- **Commands**:
  - `/start`: Begins periodic host monitoring and notifications.
  - `/stop`: Stops the monitoring task.
  - `/status`: Runs an `nmap` scan on all hosts and returns filtered results.
- **Configuration**: Uses environment variables and a `hosts.txt` file for easy setup.

## Prerequisites
- **Telegram Bot Token**: Obtain a bot token from [BotFather](https://t.me/BotFather).
- **Dependencies**: Requires `nmap` and `ping` installed on the system (`/bin/nmap` and `ping` commands).
- **Operating System**: Tested on Linux; paths differ in debug vs. production modes.

## Setup (Docker)
1. **Set environment variables**:
   
   Replace TELOXIDE_TOKEN with your Telegram bot token and BOT_PASSWORD with a password for bot access inside docker-compose.yml
   
2. **Modify a `hosts.txt` File**:

   In the project root `hosts.txt` add one host IP or hostname per line:
   ```
   192.168.1.1
   example.com
   ```

3. **Set Environment Variables**:
   Create a `.env` file in the project root:
   ```plaintext
   BOT_TOKEN=your_telegram_bot_token
   BOT_PASSWORD=your_secure_password
   RUST_LOG=info
   ```
   Replace `your_telegram_bot_token` with your Telegram bot token and `your_secure_password` with a password for bot access.

4. **Build and Run**:
   ```
   docker-compose up -d --build
   ```




## Usage

12. **Interact with the Bot**:
   - Start a chat with your bot on Telegram.
   - Send the password (from `.env`) to gain access.
   - Use commands:
     - `/start`: Start monitoring hosts every 60 seconds.
     - `/stop`: Stop monitoring.
     - `/status`: Run an `nmap` scan and view results (first line of each result removed, empty lines filtered).

3. **Logs**:
   - Logs are output to the console with `RUST_LOG=info`.

## Project Structure
- **`src/main.rs`**: Main bot logic, including Telegram command handling, host monitoring, and `nmap` scans.
- **`hosts.txt`**: List of hosts to monitor.

## Notes
- **Production Path**: In release mode, `hosts.txt` is read from `/etc/notification_bot/hosts.txt`. Ensure this directory exists and is readable.
- **Error Handling**: The bot logs errors for failed `nmap` or `ping` commands and dialogue updates.
- **Customization**:
  - Adjust `PING_INTERVAL` (default: 60 seconds) in the code to change monitoring frequency.
  - Modify `nmap` arguments in the `/status` command for different scan types or timeouts.

# TODO List
- [x] Change status command to nmap instead of ping to see running services.
- example: nmap -oG scan -Pn -T5 192.168.69.200 --host-timeout 5

- [ ] Add functionality to add/remove hosts with /add /remove commands