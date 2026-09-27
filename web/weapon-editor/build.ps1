# Rebuild the public weapon-editor bundle (readable sources stay in this private tree).
# Usage from repo root:  powershell -File web/weapon-editor/build.ps1
$ErrorActionPreference = "Stop"
$here = Split-Path -Parent $MyInvocation.MyCommand.Path
$repo = Split-Path (Split-Path $here)
$public = Join-Path (Split-Path $repo) "WormForge-public"
if (-not (Test-Path $public)) { $public = "C:\Users\Adw11\Desktop\WormForge-public" }
$out = Join-Path $public "editor.min.js"
npx --yes terser `
  (Join-Path $here "wep_schema.js") `
  (Join-Path $here "aim_bake.js") `
  (Join-Path $here "app.js") `
  -c -m -o $out
Write-Host "Wrote $out ($((Get-Item $out).Length) bytes)"
