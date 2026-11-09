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
- **Utterance**: something a participant said, in chat (a message, an edit, or an emoji reaction on
  someone else's message) or out loud in the monitored voice channel (a transcribed speech segment).
- **Hostile** (in the code; "not nice" to users): the verdict's score is at or above the guild's
  threshold.
- **Verdict**: Jev's two answers about an utterance, asked together: **aimed** (is it at or about
  a person or group, rather than a thing, nobody, or a word just mentioned) and **put-down** (is
  it an insult, mockery, sarcasm, dismissal or the like, even as a joke). Its score is the lower
  of the two, so it's only not nice when it's both.
- **Meanness** (in the code; "not niceness" to users): how mean an utterance was, 0 (friendly or neutral) to 1 (cruel or hateful), from
  a third Jev answer asked with the verdict. Shown as a percentage on `/stats` cards. Separate from the
  verdict: the score says how sure Jev is that it's not nice, meanness says how bad it was.
- **Conversation**: what was said just before an utterance in the same chat channel or voice call,
  each line with who said it and when. Messages, edits and voice lines are judged with it.
- **Call**: the voice moment an utterance was said in: the speaker, who's in the channel, and the
  conversation.
- **Timeout**: the bot's own enforcement, never Discord's native timeout: until it ends, the
 participant's messages are deleted, their reactions removed, and they're server-muted and
 deafened in VC. It works the same on everyone, the owner and admins included.
- **Monitored channel**: the one voice channel the bot always sits in and listens to.
- **Activity log**: the raw record of everything that happens, for building stats afterwards.
