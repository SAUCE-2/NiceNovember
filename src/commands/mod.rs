mod nice;
mod stats;
mod status;

use crate::app::{Data, Error};

pub fn all() -> Vec<poise::Command<Data, Error>> {
    vec![nice::nice(), stats::stats(), status::status()]
}
