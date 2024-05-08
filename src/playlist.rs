use std::error;

use serde::Deserialize;
use tokio::process::Command;

pub async fn query(url: &str) -> Result<Vec<Metadata>, Box<dyn error::Error + Send + Sync>> {
    let args = [
        "-j",
        url,
        "-f",
        "ba[abr>0][vcodec=none]/best",
        "--flat-playlist",
    ];

    let output = Command::new("yt-dlp").args(args).output().await?;
    if !output.status.success() {
        return Err("Failed querying playlist".into());
    }

    let metadata = output
        .stdout
        .split(|&b| b == b'\n')
        .map(serde_json::from_slice)
        .collect::<Result<Vec<Metadata>, serde_json::Error>>()?;

    Ok(metadata)
}

#[derive(Clone, Debug, Deserialize)]
pub struct Metadata {
    pub url: String,
    pub title: String,
}
