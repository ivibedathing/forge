# CLAUDE.md

This project is built with the Forge engine, driven entirely from the `engine`
CLI. Read `AGENTS.md` in this directory before editing a scene — it is the
orientation for working here: the edit → validate → screenshot → **look at the
PNG** loop, the CLI's stdout/stderr/exit-code contract, the scene file format,
and the conventions that are easy to get wrong.

@AGENTS.md

## Quick reference

```bash
engine validate first.json
engine screenshot first.json --out /tmp/check.png --steps 120
engine list-components | jq -r '.components | keys[]'
engine agent-guide
```

After rendering, **read the PNG**. Authoring a scene without looking at the
result is the one mistake the whole engine is shaped to prevent.
