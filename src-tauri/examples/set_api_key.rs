//! One-off helper to store the Anthropic API key in the OS keychain (Windows Credential
//! Manager), matching exactly what the app reads at runtime (see `src/secrets.rs`).
//!
//! Run:  cargo run -p tianji --example set_api_key
//! Then paste your key at the prompt. The key is read from stdin, so it does not end up in your
//! shell history.

use std::io::{self, Write};

const SERVICE: &str = "dev.tianji.app";
const ACCOUNT: &str = "anthropic";

fn main() {
    print!("Paste your Anthropic API key, then press Enter: ");
    io::stdout().flush().expect("flush stdout");

    let mut key = String::new();
    io::stdin().read_line(&mut key).expect("read stdin");
    let key = key.trim();

    if key.is_empty() {
        eprintln!("No key entered; aborting.");
        std::process::exit(1);
    }

    let entry = keyring::Entry::new(SERVICE, ACCOUNT).expect("open keychain entry");
    entry.set_password(key).expect("store key in keychain");
    println!("Stored Anthropic key (service \"{SERVICE}\", account \"{ACCOUNT}\").");
}
