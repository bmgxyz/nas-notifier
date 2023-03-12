# NAS Notifier

*A Telegram notification engine for my netbook NAS*

This is a personal project that solves my particular set of needs. I don't expect it to be useful to
anyone else, but I've posted it publicly anyway because I'm proud of it.

Once installed and configured, this simple binary will send Telegram notifications from a bot
whenever any of these conditions are met:

- a user successfully logs in from a new IP address
- there is a failed login attempt from anywhere
- any `zpool` changes its health condition

The binary runs as a `systemd` unit and is configured by setting values in a documented TOML file.

## Installation

This program assumes that the target system is a publicly-accessible Linux server running ZFS and
OpenSSH with public key authentication.

1. [Create a Telegram bot][new-bot] and record its API key for later use.
2. Start a conversation with your new bot in Telegram. This is required because bots can't start
   conversations on their own.
3. Install [Rust][rust].
4. Compile the binary with `cargo build --release` and copy the result to
   `/usr/local/bin/nas-notifier` on the target system.
5. Copy `config.example.toml` to `/etc/nas-notifier.toml` on the target system and set desired
   values. You will need your numeric Telegram user ID. Refer to the documentation comments in the
   example file for more information.
6. Copy `nas-notifier.service` to `/etc/systemd/system/nas-notifier.service`.
7. Start the service with `systemctl daemon-reload && systemctl start nas-notifier`. You must run
   this command as `root`.
8. (optional) Enable the service to run at boot with `systemctl enable nas-notifier`. You must run
   this command as `root`.

[new-bot]: https://core.telegram.org/bots/features#creating-a-new-bot
[rust]: https://rustup.rs/
