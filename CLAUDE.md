# CLAUDE.md

## Commit messages

Use [Conventional Commits](https://www.conventionalcommits.org/en/v1.0.0/) for every commit:

```
<type>(<optional scope>): <short imperative summary>

<optional body>

<optional footer(s)>
```

- Types: `feat`, `fix`, `docs`, `style`, `refactor`, `perf`, `test`, `build`, `ci`, `chore`, `revert`.
- Keep the summary lowercase, imperative, and under ~72 characters, with no trailing period.
- Mark breaking changes with `!` after the type/scope (e.g. `feat(api)!: ...`) and/or a `BREAKING CHANGE:` footer.

## How to make changes

Follow the `ub-mode` skill (`.claude/skills/ub-mode/SKILL.md`, vendored from
[unclebob/ub-mode](https://github.com/unclebob/ub-mode) at `3da62bf`) for every code change, not
only when `/ub-mode` is invoked. Its quality gates ([crapper](https://github.com/unclebob/crapper),
[dryer](https://github.com/unclebob/dryer), [mutator](https://github.com/unclebob/mutator)) all
support Rust.

One exception to the skill: instead of pushing to `main`, push to a branch and open a pull request,
since CI runs on PRs and every push to `main` publishes the image the bot auto-updates from.

## Glossary

- **Participant**: a member holding the guild's participant role. Only participants are judged
  and timed out.
- **Utterance**: something a participant said, in chat (a message or an edit) or out loud in the
  monitored voice channel (a transcribed speech segment).
- **Hostile**: Jev scored the utterance at or above the guild's threshold.
- **Native timeout**: Discord's own timeout. **Forced timeout**: the bot's own enforcement for
  members Discord won't let it time out (the owner, admins, members above the bot's role).
  Every timeout is also enforced by the bot (messages deleted, reactions removed, server-muted in
  VC), so both kinds behave the same.
- **Monitored channel**: the one voice channel the bot always sits in and listens to.
- **Activity log**: the raw record of everything that happens, for building stats afterwards.
