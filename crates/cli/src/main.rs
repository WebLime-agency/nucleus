use anyhow::Context;
use clap::{Parser, Subcommand};
use nucleus_core::DEFAULT_DAEMON_ADDR;
use nucleus_protocol::HealthResponse;

#[derive(Debug, Parser)]
#[command(name = "nucleus")]
#[command(about = "Nucleus operator CLI")]
struct Args {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Health {
        #[arg(long, default_value = DEFAULT_DAEMON_ADDR)]
        bind: String,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    match args.command {
        Command::Health { bind } => {
            let url = format!("http://{bind}/health");
            let response = reqwest::get(&url)
                .await
                .with_context(|| format!("failed to reach {url}"))?
                .error_for_status()
                .with_context(|| format!("health endpoint returned an error for {url}"))?
                .json::<HealthResponse>()
                .await
                .context("failed to decode health response")?;

            println!(
                "{} {} {}",
                response.service, response.version, response.status
            );
        }
    }

    Ok(())
}
