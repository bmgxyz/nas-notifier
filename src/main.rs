use std::{
    collections::HashMap,
    error::Error,
    io::{Read, Seek},
    net::Ipv4Addr,
    time::Duration,
};

use libzetta::zpool::{Health, ZpoolEngine, ZpoolOpen3};
use log::{debug, info, trace};
use serde::Deserialize;
use simple_logger::SimpleLogger;

type NasResult<T> = Result<T, Box<dyn Error>>;

struct NasNotifier {
    config: Config,
    auth_log_pos: u64,
    zpools_health: HashMap<String, Health>,
    first_loop: bool,
}

#[derive(Deserialize)]
struct Config {
    #[serde(rename = "poll-duration-seconds")]
    poll_duration_seconds: u64,
    telegram: TelegramConfig,
    notifications: NotificationsConfig,
}

#[derive(Deserialize)]
struct TelegramConfig {
    #[serde(rename = "user-id")]
    user_id: i64,
    hostname: String,
    #[serde(rename = "api-key")]
    api_key: String,
}

#[derive(Deserialize, Clone)]
struct NotificationsConfig {
    #[serde(rename = "new-login-ip")]
    new_login_ip: Option<bool>,
    #[serde(rename = "known-ips")]
    known_ips: Option<Vec<String>>,
    #[serde(rename = "failed-login")]
    failed_login: Option<bool>,
    #[serde(rename = "pool-health")]
    pool_health: Option<bool>,
}

fn zpool_health_to_string(health: &Health) -> String {
    match *health {
        Health::Available => "AVAILABLE".to_string(),
        Health::Degraded => "DEGRADED".to_string(),
        Health::Faulted => "FAULTED".to_string(),
        Health::Offline => "OFFLINE".to_string(),
        Health::Online => "ONLINE".to_string(),
        Health::Removed => "REMOVED".to_string(),
        Health::Unavailable => "UNAVAILABLE".to_string(),
    }
}

impl Default for NasNotifier {
    fn default() -> Self {
        NasNotifier {
            config: Config {
                poll_duration_seconds: 30,
                telegram: TelegramConfig {
                    user_id: 0,
                    hostname: String::new(),
                    api_key: String::new(),
                },
                notifications: NotificationsConfig {
                    new_login_ip: None,
                    known_ips: None,
                    failed_login: None,
                    pool_health: None,
                },
            },
            auth_log_pos: 0,
            zpools_health: HashMap::new(),
            first_loop: true,
        }
    }
}

impl NasNotifier {
    const CONFIG_FILE_PATH: &str = "/etc/nas-notifier.toml";
    const AUTH_LOG_FILE_PATH: &str = "/var/log/auth.log";

    fn new() -> NasResult<Self> {
        SimpleLogger::new().env().without_timestamps().init()?;
        debug!("Reading config file at {}", Self::CONFIG_FILE_PATH);
        let config_file = std::fs::read_to_string(Self::CONFIG_FILE_PATH)?;
        let config: Config = toml::from_str(&config_file)?;
        debug!("Successfully read and parsed config file");
        Ok(NasNotifier {
            config,
            ..Default::default()
        })
    }
    fn run(mut self) -> NasResult<()> {
        let new_login_ip = self.config.notifications.new_login_ip.unwrap_or(false);
        let known_ips = self
            .config
            .notifications
            .known_ips
            .clone()
            .unwrap_or_default();
        let failed_login = self.config.notifications.failed_login.unwrap_or(false);
        let pool_unhealthy = self.config.notifications.pool_health.unwrap_or(false);
        let zfs_handle = ZpoolOpen3::default();
        info!("Startup complete, beginning polling loop");

        // Periodically poll all data sources, process them, and send notifications as needed.
        loop {
            debug!("New polling loop");
            if new_login_ip || failed_login {
                // Get new lines from /var/log/auth.log.
                debug!("Getting new lines from {}", Self::AUTH_LOG_FILE_PATH);
                trace!("auth_log_pos: {}", self.auth_log_pos);
                let mut auth_log = std::fs::File::open(Self::AUTH_LOG_FILE_PATH)?;
                // Skip all existing lines if this is the first loop
                if self.first_loop {
                    trace!("first loop, skipping to end of auth log");
                    self.auth_log_pos = auth_log.metadata()?.len();
                    self.first_loop = false;
                }
                if auth_log.metadata()?.len() < self.auth_log_pos {
                    trace!("file length is less than auth_log_pos, setting to zero");
                    self.auth_log_pos = 0;
                }
                auth_log.seek(std::io::SeekFrom::Start(self.auth_log_pos))?;
                let mut new_auth_lines = String::new();
                let bytes_read = auth_log.read_to_string(&mut new_auth_lines)?;
                self.auth_log_pos += bytes_read as u64;
                trace!("bytes_read: {}", bytes_read);
                trace!("new_auth_lines.len(): {}", new_auth_lines.len());
                trace!("auth_log_pos: {}", self.auth_log_pos);

                // Parse new lines in auth.log and send notifications as needed.
                for new_line in new_auth_lines.lines() {
                    if new_login_ip
                        && new_line.contains("sshd")
                        && new_line.contains("Accepted publickey for")
                    {
                        let ip_addresses: Vec<Ipv4Addr> = new_line
                            .split_ascii_whitespace()
                            .filter_map(|w| w.parse().ok())
                            .collect();
                        for ip in ip_addresses {
                            if !ip.is_private() && !known_ips.contains(&ip.to_string()) {
                                info!("Found new login IP, sending notification");
                                self.send_notification(
                                    &format!("There was a successful login on `{}` from an unknown IP address. Here's the relevant line from `{}`:\n\n`{}`",
                                        self.config.telegram.hostname,
                                        Self::AUTH_LOG_FILE_PATH,
                                        new_line.trim_end()
                                    )
                                )?;
                                info!("Notification sent");
                            } else {
                                debug!("Found new login, but the IP is private or whitelisted");
                            }
                        }
                    }
                    if failed_login
                        && new_line.contains("sshd")
                        && new_line.contains("Connection closed by authenticating user")
                    {
                        info!("Found failed login, sending notification");
                        self.send_notification(
                            &format!("There was a failed login attempt on `{}`. Here's the relevant line from `{}`:\n\n`{}`",
                                self.config.telegram.hostname,
                                Self::AUTH_LOG_FILE_PATH,
                                new_line.trim_end()
                            )
                        )?;
                        info!("Notification sent");
                    }
                }
            }

            if pool_unhealthy {
                // Check zpool health and send notifications for any changes in health status. Note
                // that this does not clear destroyed zpools from memory. If this is an issue, then
                // one solution would be to restart the program.
                debug!("Getting zpool statuses");
                let status = zfs_handle.all()?;
                debug!("Got zpool statuses");
                for zpool in status {
                    let name = zpool.name().to_owned();
                    if self.zpools_health.contains_key(&name) {
                        // We already know about this zpool, so send a message if its health has
                        // changed.
                        let previous_state = self.zpools_health.get(&name).unwrap();
                        let current_state = zpool.health();
                        if previous_state != current_state {
                            info!("Detected a zpool health status change for '{}' (new health status: '{}'), sending notification",
                                name,
                                zpool_health_to_string(current_state));
                            self.send_notification(&format!(
                                "Zpool `{}` entered the `{}` state.",
                                name,
                                zpool_health_to_string(current_state)
                            ))?;
                            info!("Notification sent");
                            self.zpools_health.insert(name, current_state.to_owned());
                        }
                    } else {
                        // We haven't seen this zpool before, so add it to the hashmap.
                        info!("Found new zpool '{}'", name);
                        let health = zpool.health().to_owned();
                        self.zpools_health.insert(name, health);
                    }
                }
            }
            // Sleep until it's time to poll again.
            debug!("Sleeping for {} seconds", self.config.poll_duration_seconds);
            std::thread::sleep(Duration::from_secs(self.config.poll_duration_seconds));
        }
    }
    fn send_notification(&self, text: &str) -> NasResult<()> {
        let url = format!(
            "https://api.telegram.org/bot{}/sendMessage",
            self.config.telegram.api_key
        );
        let mut payload = HashMap::new();
        let user_id = self.config.telegram.user_id.to_string();
        payload.insert("chat_id", user_id.as_str());
        payload.insert("text", text);
        payload.insert("parse_mode", "Markdown");
        let client = reqwest::blocking::Client::new();
        client.post(url).json(&payload).send()?;
        Ok(())
    }
}

fn main() -> NasResult<()> {
    NasNotifier::new()?.run()
}
