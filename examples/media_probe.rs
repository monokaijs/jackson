use anyhow::{Context, Result};
use songbird::input::{
    Input, YoutubeDl,
    codecs::{get_codec_registry, get_probe},
};

#[tokio::main]
async fn main() -> Result<()> {
    let query = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "Rick Astley Never Gonna Give You Up".to_owned());
    let client = reqwest::Client::builder()
        .user_agent("jackson-media-probe")
        .build()?;
    let input: Input = YoutubeDl::new_search(client, query).into();

    input
        .make_playable_async(get_codec_registry(), get_probe())
        .await
        .context("resolved media could not be opened by Songbird")?;

    println!("media stream resolved and demuxed successfully");
    Ok(())
}
