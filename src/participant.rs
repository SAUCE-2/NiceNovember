//! Who takes part: members holding the guild's participant role. Only they are judged.

use anyhow::Result;
use poise::serenity_prelude::{self as serenity, GuildId, RoleId, UserId};

use crate::app::Data;

/// Does a member with `roles` hold the participant role (`None` = none set, so nobody does)?
pub fn holds(participant_role: Option<i64>, roles: &[RoleId]) -> bool {
    participant_role.is_some_and(|r| roles.contains(&RoleId::new(r as u64)))
}

/// Is `user` a participant? `roles` saves a lookup when the event already has them.
pub async fn is_participant(
    ctx: &serenity::Context,
    data: &Data,
    guild_id: GuildId,
    user: UserId,
    roles: Option<&[RoleId]>,
) -> Result<bool> {
    let role = data.db.guild(guild_id.get()).await?.participant_role_id;
    if role.is_none() {
        return Ok(false);
    }
    Ok(match roles {
        Some(r) => holds(role, r),
        None => guild_id
            .member(ctx, user)
            .await
            .is_ok_and(|m| holds(role, &m.roles)),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn needs_the_role() {
        let roles = [RoleId::new(1), RoleId::new(2)];
        assert!(holds(Some(2), &roles));
        assert!(!holds(Some(3), &roles));
        assert!(!holds(None, &roles), "no role set: nobody takes part");
        assert!(!holds(Some(1), &[]));
    }
}
