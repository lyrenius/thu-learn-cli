mod api;
mod auth;
mod browser_login;
mod cache;
mod cli;
mod client;
mod credentials;
#[cfg(test)]
mod mock_sso;
mod models;
mod paths;
mod sso;
mod tui;

#[tokio::main]
async fn main() {
    if let Err(e) = cli::run().await {
        eprintln!("Error: {e:#}");
        std::process::exit(1);
    }
}
