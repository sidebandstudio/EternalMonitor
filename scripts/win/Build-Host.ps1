param([switch]$Release, [switch]$Test, [switch]$Lint)
$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$env:FFMPEG_DIR = 'D:\AgentWork\sdk\ffmpeg-7.1.1-full_build-shared'
$env:LIBCLANG_PATH = 'D:\AgentWork\LLVM\bin'
$env:CARGO_HOME = 'D:\AgentWork\cargo'
$env:RUSTUP_HOME = 'D:\AgentWork\rustup'
$env:CARGO_TARGET_DIR = 'D:\AgentWork\Eternal-Monitor\target'
$env:TEMP = 'D:\AgentWork\temp'
$env:TMP = $env:TEMP
$env:PATH = "$env:FFMPEG_DIR\bin;$env:USERPROFILE\.cargo\bin;" + $env:PATH
$vsRoot = 'C:\Program Files\Microsoft Visual Studio\2022\Community'
Import-Module "$vsRoot\Common7\Tools\Microsoft.VisualStudio.DevShell.dll"
Enter-VsDevShell -VsInstallPath $vsRoot -SkipAutomaticLocation -DevCmdArguments '-arch=x64 -host_arch=x64'
$env:BINDGEN_EXTRA_CLANG_ARGS = (($env:INCLUDE -split ';' | Where-Object { $_ } | ForEach-Object { '-isystem "' + $_ + '"' }) -join ' ')
Set-Location 'D:\AgentWork\Eternal-Monitor'
New-Item -ItemType Directory -Force 'D:\AgentWork\em-v030\logs', $env:TEMP | Out-Null
$toolchain = [regex]::Match((Get-Content rust-toolchain.toml -Raw), 'channel\s*=\s*"([^"]+)"').Groups[1].Value
if (!$toolchain) { throw 'Missing pinned toolchain' }
rustup toolchain install $toolchain --profile minimal --component rustfmt --component clippy --no-self-update
if ($LASTEXITCODE -ne 0) { throw 'Rust toolchain installation failed' }
$log = 'D:\AgentWork\em-v030\logs\build-' + (Get-Date -Format 'yyyyMMdd-HHmmss') + '.txt'
Start-Transcript -Path $log -Force | Out-Null
try {
    rustc --version
    $buildArgs = @('build','-p','eternal-host','--locked')
    if ($Release) { $buildArgs += '--release' }
    & cargo @buildArgs
    if ($LASTEXITCODE -ne 0) { throw 'cargo build failed' }
    if ($Lint) {
        cargo clippy --workspace --all-targets --locked -- -D warnings
        if ($LASTEXITCODE -ne 0) { throw 'cargo clippy failed' }
    }
    if ($Test) {
        cargo test --workspace --locked
        if ($LASTEXITCODE -ne 0) { throw 'cargo test failed' }
    }
    $profile = if ($Release) { 'release' } else { 'debug' }
    Write-Host "Binary: $env:CARGO_TARGET_DIR\$profile\eternal-host.exe"
    Write-Host "C: free bytes: $((Get-PSDrive C).Free)"
} finally { Stop-Transcript | Out-Null }
