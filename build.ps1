$ErrorActionPreference = "Stop"
function Step($cmd) { & $cmd; if ($LASTEXITCODE -ne 0) { throw "failed: $cmd" } }

# The UI is one self-contained page: Node only inlines its CSS, no npm packages.
Push-Location ui; Step { node build.mjs }; Pop-Location

# 64-bit app, in-game DLL and FlowScale SR; then the 32-bit app and DLL for 32-bit games.
# The WebView2 loader is linked statically, so no WebView2Loader.dll ships.
Step { cargo build --release --workspace }
rustup target add i686-pc-windows-msvc | Out-Null
Step { cargo build --release --target i686-pc-windows-msvc --lib --bin flowscale }

New-Item -ItemType Directory -Force dist | Out-Null
Copy-Item target\release\flowscale.exe, target\release\flowscale_hook.dll, target\release\flowscale_sr.dll dist\
Copy-Item target\i686-pc-windows-msvc\release\flowscale.exe dist\flowscale32.exe
Copy-Item target\i686-pc-windows-msvc\release\flowscale_hook.dll dist\flowscale_hook32.dll
Copy-Item LICENSE dist\LICENSE.txt
Copy-Item -Recurse -Force licenses dist\
Remove-Item -ErrorAction SilentlyContinue dist\WebView2Loader.dll

# Release package: what FlowScale needs to run, plus its licenses.
$version = (Select-String -Path Cargo.toml -Pattern '^version = "(.+)"' -List).Matches[0].Groups[1].Value
$files = "flowscale.exe", "flowscale32.exe", "flowscale_hook.dll", "flowscale_hook32.dll", "flowscale_sr.dll", "LICENSE.txt", "licenses"
Compress-Archive -Force -Path ($files | ForEach-Object { "dist\$_" }) -DestinationPath "dist\FlowScale-$version.zip"
Get-ChildItem dist\FlowScale-$version.zip, dist\*.exe, dist\*.dll | Format-Table Name, Length
