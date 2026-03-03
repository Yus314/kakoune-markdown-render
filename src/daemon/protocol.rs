use std::io::BufRead;
use anyhow::{bail, Context};

pub struct PingMsg {
    pub session:         String,
    pub bufname:         String,
    pub timestamp:       u64,
    pub cursor:          usize,
    pub width:           usize,
    /// `"{ts:016x}:{hash:016x}"` 形式。kak_opt_mkdr_last_config_hash の生値をそのまま格納。
    pub config_hash_str: String,
    pub client:          String,
    #[allow(dead_code)]  // プロトコルで受信するが現在未使用（将来の cmd_fifo 応答用に保持）
    pub cmd_fifo:        String,
}

pub struct RenderMsg {
    pub session:   String,
    pub bufname:   String,
    pub timestamp: u64,
    pub cursor:    usize,
    pub width:     usize,
    pub client:    String,
    #[allow(dead_code)]  // プロトコルで受信するが現在未使用（将来の cmd_fifo 応答用に保持）
    pub cmd_fifo:  String,
    pub config:    Vec<u8>,
    pub content:   String,
}

pub struct CloseMsg {
    #[allow(dead_code)]  // プロトコルで受信するが CLOSE 処理は bufname のみ使用
    pub session: String,
    pub bufname: String,
}

pub enum Message {
    Ping(PingMsg),
    Render(RenderMsg),
    Close(CloseMsg),
    Shutdown(#[allow(dead_code)] String),
}

impl Message {
    pub fn bufname(&self) -> &str {
        match self {
            Message::Ping(m)     => &m.bufname,
            Message::Render(m)   => &m.bufname,
            Message::Close(m)    => &m.bufname,
            Message::Shutdown(_) => "",
        }
    }
    pub fn width(&self) -> usize {
        match self {
            Message::Ping(m)   => m.width,
            Message::Render(m) => m.width,
            _                  => 0,
        }
    }
}

/// UDS stream から1メッセージを読む（接続ごとに呼ぶ）
pub fn parse_message(mut reader: impl BufRead) -> anyhow::Result<Message> {
    let mut header = String::new();
    let n = reader.read_line(&mut header)?;
    if n == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "connection closed without sending data (check_alive)",
        ).into());
    }

    let header = header.trim_end_matches('\n').trim_end_matches('\r');
    let fields: Vec<&str> = header.split('\t').collect();

    match fields.first().copied() {
        Some("PING") => {
            if fields.len() < 9 {
                bail!("PING: expected 9 fields, got {}: {:?}", fields.len(), fields);
            }
            Ok(Message::Ping(PingMsg {
                session:         fields[1].to_string(),
                bufname:         fields[2].to_string(),
                timestamp:       fields[3].parse().context("PING timestamp")?,
                cursor:          fields[4].parse().context("PING cursor")?,
                width:           fields[5].parse().context("PING width")?,
                config_hash_str: fields[6].to_string(),
                client:          fields[7].to_string(),
                cmd_fifo:        fields[8].to_string(),
            }))
        }
        Some("RENDER") => {
            if fields.len() < 9 {
                bail!("RENDER: expected 9 fields, got {}: {:?}", fields.len(), fields);
            }
            let session   = fields[1].to_string();
            let bufname   = fields[2].to_string();
            let timestamp = fields[3].parse().context("RENDER timestamp")?;
            let cursor    = fields[4].parse().context("RENDER cursor")?;
            let width     = fields[5].parse().context("RENDER width")?;
            let client    = fields[6].to_string();
            let cmd_fifo  = fields[7].to_string();
            let config_len: usize = fields[8].parse().context("RENDER config_len")?;

            let mut config = vec![0u8; config_len];
            reader.read_exact(&mut config)?;

            // content は接続 EOF まで読む
            let mut content_bytes = Vec::new();
            reader.read_to_end(&mut content_bytes)?;

            // UTF-8 検証（失敗した場合は lossy 変換）
            let content = match String::from_utf8(content_bytes) {
                Ok(s)  => s,
                Err(e) => String::from_utf8_lossy(e.as_bytes()).into_owned(),
            };

            Ok(Message::Render(RenderMsg {
                session, bufname, timestamp, cursor, width, client, cmd_fifo, config, content,
            }))
        }
        Some("CLOSE") => {
            if fields.len() < 3 {
                bail!("CLOSE: expected 3 fields, got {}", fields.len());
            }
            Ok(Message::Close(CloseMsg {
                session: fields[1].to_string(),
                bufname: fields[2].to_string(),
            }))
        }
        Some("SHUTDOWN") => {
            if fields.len() < 2 {
                bail!("SHUTDOWN: expected 2 fields, got {}", fields.len());
            }
            Ok(Message::Shutdown(fields[1].to_string()))
        }
        other => bail!("unknown message type: {:?}", other),
    }
}
