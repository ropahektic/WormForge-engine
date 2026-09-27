# Web weapon editor (source of truth)

Readable JS for https://ropahektic.github.io/WormForge/ lives here so the
public WormForge repo does not ship `wep_schema.js` with field offsets in
plain text.

- Edit `wep_schema.js` / `aim_bake.js` / `app.js` in this folder.
- Run `powershell -File web/weapon-editor/build.ps1` to refresh
  `../WormForge-public/editor.min.js` (minified bundle only).
- Commit the private sources here; commit only `editor.min.js` + `index.html`
  on the public site.
