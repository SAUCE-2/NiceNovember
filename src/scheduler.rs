//! Background work: loops releasing ended timeouts and watching the voice monitor, and rating the
//! meanness of earlier judgements once at startup.

use std::time::Duration;

use anyhow::Result;
use poise::serenity_prelude as serenity;

use crate::app::Data;
use crate::voice::monitor;
use crate::{rate, timeout};

pub fn spawn_loops(ctx: serenity::Context, data: Data) {
    let (c, d) = (ctx.clone(), data.clone());
    tokio::spawn(async move {
        if let Err(e) = rate::rate_unrated(&c, &d).await {
            tracing::warn!("rating earlier judgements stopped: {e:#}");
        }
    });
    let (c, d) = (ctx.clone(), data.clone());
    tokio::spawn(every(Duration::from_secs(1), move || {
        let (c, d) = (c.clone(), d.clone());
        async move { timeout::sweep(&c, &d).await }
    }));
    tokio::spawn(every(Duration::from_secs(30), move || {
        let (c, d) = (ctx.clone(), data.clone());
        async move { monitor::ensure_all(&c, &d).await }
    }));
}

async fn every<F, Fut>(period: Duration, mut f: F)
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<()>>,
{
    let mut interval = tokio::time::interval(period);
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    loop {
        interval.tick().await;
        if let Err(e) = f().await {
            tracing::warn!("background loop error: {e:#}");
        }
    }
}
