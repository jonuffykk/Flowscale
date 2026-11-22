$ErrorActionPreference = "Stop"
function Step($cmd) { & $cmd; if ($LASTEXITCODE -ne 0) { throw "failed: $cmd" } }
# UI: a single self-contained page, no npm dependencies (only Node to inline the CSS).
Push-Location ui; Step { node build.mjs }; Pop-Location
Step { cargo build --release --workspace }
rustup target add i686-pc-windows-msvc | Out-Null
Step { cargo build --release --target i686-pc-windows-msvc --lib --bin flowscale }
New-Item -ItemType Directory -Force dist | Out-Null
Copy-Item target\release\flowscale.exe, target\release\flowscale_hook.dll, target\release\flowscale_sr.dll, target\release\WebView2Loader.dll dist\
Copy-Item -Recurse -Force licenses dist\
Copy-Item LICENSE dist\LICENSE.txt
Copy-Item target\i686-pc-windows-msvc\release\flowscale.exe dist\flowscale32.exe
Copy-Item target\i686-pc-windows-msvc\release\flowscale_hook.dll dist\flowscale_hook32.dll
# WebView2 now keeps its cache in %LOCALAPPDATA%\FlowScale; drop the old in-folder copy.
if (Test-Path dist\webview-data) { Remove-Item -Recurse -Force dist\webview-data }
Get-ChildItem dist -File | Format-Table Name, Length
# Release package: only what FlowScale needs to run, plus its licenses.
$version = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"' -List).Matches[0].Groups[1].Value
$files = "flowscale.exe", "flowscale32.exe", "flowscale_hook.dll", "flowscale_hook32.dll", "flowscale_sr.dll", "WebView2Loader.dll", "LICENSE.txt", "licenses"
Compress-Archive -Force -Path ($files | ForEach-Object { "dist\$_" }) -DestinationPath "dist\FlowScale-$version.zip"
