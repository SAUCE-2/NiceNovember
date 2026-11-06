mod ai;
mod app;
mod commands;
mod config;
mod db;
mod events;
mod hostility;
mod judge;
mod participant;
mod scheduler;
mod setup;
mod stats;
mod timeout;
mod voice;

use anyhow::Result;
use poise::serenity_prelude::{self as serenity, GatewayIntents, GuildId};
use songbird::SerenityInit;
use songbird::driver::{Channels, DecodeConfig, DecodeMode, SampleRate};

use crate::app::{Data, Error, Inner};

#[tokio::main]
async fn main() -> Result<()> {
    // reqwest pulls in aws-lc-rs and the voice stack pulls in ring; rustls panics on first TLS use
    // unless one is picked explicitly.
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .ok();
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,serenity=warn,songbird=warn".into()),
        )
        .init();

    let cfg = config::Config::from_env()?;
    let db = db::Db::connect(&cfg.database_url).await?;
    let stt = match voice::stt::Stt::load(&cfg.whisper_model) {
        Ok(s) => Some(s),
        Err(e) => {
            tracing::warn!("voice can't be judged: {e:#}");
            None
        }
    };
    let jev = cfg.jev_api_key.clone().map(ai::jev::Jev::new);
    if jev.is_none() {
        tracing::warn!("JEV_API_KEY not set: nothing can be judged");
    }
    let data = Data::new(Inner {
        cfg: cfg.clone(),
        db,
        stt,
        jev,
        monitor: Default::default(),
        vc_sessions: Default::default(),
        last_presence: Default::default(),
        started_at: chrono::Utc::now(),
    });

    let framework = poise::Framework::builder()
        .options(poise::FrameworkOptions {
            commands: commands::all(),
            event_handler: |fw, event| {
                Box::pin(
                    async move { events::handle(fw.serenity_context, event, fw.user_data).await },
                )
            },
            on_error: |err| Box::pin(on_error(err)),
            ..Default::default()
        })
        .setup(move |ctx, ready, framework| {
            Box::pin(async move {
                let cmds = &framework.options().commands;
                match cfg.dev_guild_id {
                    Some(g) => {
                        poise::builtins::register_in_guild(ctx, cmds, GuildId::new(g)).await?
                    }
                    None => poise::builtins::register_globally(ctx, cmds).await?,
                }
                tracing::info!("logged in as {}", ready.user.name);
                scheduler::spawn_loops(ctx.clone(), data.clone());
                Ok(data)
            })
        })
        .build();

    // Decode received voice straight to what Whisper wants: 16 kHz mono.
    let voice_cfg = songbird::Config::default().decode_mode(DecodeMode::Decode(DecodeConfig::new(
        Channels::Mono,
        SampleRate::Hz16000,
    )));

    let intents = GatewayIntents::non_privileged()
        | GatewayIntents::MESSAGE_CONTENT
        | GatewayIntents::GUILD_MEMBERS
        | GatewayIntents::GUILD_PRESENCES;
    serenity::ClientBuilder::new(&cfg.discord_token, intents)
        .framework(framework)
        .register_songbird_from_config(voice_cfg)
        .await?
        .start()
        .await?;
    Ok(())
}

async fn on_error(err: poise::FrameworkError<'_, Data, Error>) {
    match err {
        poise::FrameworkError::Command { error, ctx, .. } => {
            tracing::warn!(command = ctx.command().name.as_ref(), "{error:#}");
            let msg = poise::CreateReply::default()
                .content(format!("❌ {error}"))
                .ephemeral(true);
            ctx.send(msg).await.ok();
        }
        other => {
            if let Err(e) = poise::builtins::on_error(other).await {
                tracing::error!("error while handling error: {e}");
            }
        }
    }
}
