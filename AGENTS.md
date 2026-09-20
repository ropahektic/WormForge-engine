# Working in this checkout

Read `contributing-with-an-agent.md` before changing weapons or engine verbs.

## Local deployment preference

The user has requested installation after future WormForge/sentry changes, once
the relevant checks and Windows DLL build succeed. Use:

```sh
python3 tools/install-local.py
```

This installs `wkLua.dll` and `mods/examples/sentry_gun` into the user's game:

`/Users/eule/Applications/Worms Armageddon.app/Contents/SharedSupport/prefix/drive_c/GOG Games/Worms Armageddon`

The script backs up existing copies under ignored `target/local-install-backups/`
and verifies the installed files. It leaves other mods alone. Installation is
already authorized; request only the sandbox escalation needed to write there.
Do not close or restart the user's running game automatically. Remind them to
restart WA to load a changed DLL. This preference does not authorize pushing or
committing changes.
