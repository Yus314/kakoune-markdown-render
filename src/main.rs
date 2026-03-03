use clap::{Parser, Subcommand};
use anyhow::Result;

mod config;
mod daemon;
mod kak;
mod offset;
mod paths;
mod render;
mod send;

#[derive(Parser)]
#[command(name = "mkdr", about = "Kakoune markdown renderer")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// デーモン起動（UDS で待ち受け、2スレッド構成）
    Daemon {
        #[arg(long)]
        session: String,
    },

    /// daemon に PING または RENDER を送信
    Send {
        #[arg(long)]
        session: String,

        #[arg(long)]
        bufname: Option<String>,

        #[arg(long)]
        timestamp: Option<u64>,

        #[arg(long)]
        cursor: Option<usize>,

        #[arg(long)]
        width: Option<usize>,

        #[arg(long)]
        client: Option<String>,

        #[arg(long)]
        cmd_fifo: Option<String>,

        /// PING モード
        #[arg(long)]
        ping: bool,

        /// 接続確認のみ（成否を終了コードで返す）
        #[arg(long)]
        check_alive: bool,

        /// バッファ状態を daemon から解放
        #[arg(long)]
        close: bool,

        /// daemon を停止
        #[arg(long)]
        shutdown: bool,

        /// PING 時の config キャッシュ文字列（`"{ts:016x}:{hash:016x}"` 形式）
        #[arg(long)]
        config_hash: Option<String>,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Commands::Daemon { session } => {
            daemon::run(&session)?;
        }
        Commands::Send {
            session, bufname, timestamp, cursor, width, client, cmd_fifo,
            ping, check_alive, close, shutdown, config_hash,
        } => {
            let is_check_alive = check_alive;
            let args = send::SendArgs {
                session, bufname, timestamp, cursor, width, client, cmd_fifo,
                ping, check_alive, close, shutdown, config_hash,
            };
            // check_alive の失敗は exit code 1 として伝える（kak の shell から参照）
            if let Err(e) = send::run_send(&args) {
                if is_check_alive {
                    std::process::exit(1);
                }
                return Err(e);
            }
        }
    }
    Ok(())
}
