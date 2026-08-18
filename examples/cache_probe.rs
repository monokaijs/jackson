use anyhow::{Context, Result};
use serenity::all::UserId;
use songbird::input::{
    Input,
    cached::Memory,
    codecs::{get_codec_registry, get_probe},
};

use jackson::resolver::Resolver;

#[tokio::main]
async fn main() -> Result<()> {
    let query = std::env::args()
        .nth(1)
        .unwrap_or_else(|| "https://www.youtube.com/watch?v=eoJecvGMR6E".to_owned());
    let client = reqwest::Client::builder()
        .user_agent("jackson-cache-probe")
        .build()?;
    let resolver = Resolver::new(client, 1, None, None);
    let track = resolver
        .resolve(&query, UserId::new(1))
        .await?
        .into_iter()
        .next()
        .context("resolver returned no tracks")?;
    let source = resolver.input(&track)?;
    let cache = Memory::new(source)
        .await
        .context("failed to prepare cached audio input")?;
    let input: Input = cache.into();
    input
        .make_playable_async(get_codec_registry(), get_probe())
        .await
        .context("cached audio input could not be parsed")?;

    println!("cached audio input prepared and parsed successfully");
    Ok(())
}
