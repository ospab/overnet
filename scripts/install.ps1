# Установщик overnet для Windows.
#
#   irm https://raw.githubusercontent.com/ospab/overnet/master/scripts/install.ps1 | iex
#   & ([scriptblock]::Create((irm …/install.ps1))) -ConfigUrl https://…/config.json
#
# Права администратора не нужны: бинарник — в %LOCALAPPDATA%\Programs\overnet
# (добавляется в PATH пользователя), конфиг — в %LOCALAPPDATA%\overnet\config.json,
# где overnet ищет его без --config. Повторный запуск обновляет бинарник.
param(
    [string]$Version = "",
    [string]$ConfigUrl = "",
    [switch]$Uninstall
)

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"
$repo = "ospab/overnet"
$InstallDir = Join-Path $env:LOCALAPPDATA "Programs\overnet"
$DataDir = if ($env:OVERNET_HOME) { $env:OVERNET_HOME } else { Join-Path $env:LOCALAPPDATA "overnet" }
$ConfigFile = Join-Path $DataDir "config.json"

Write-Host "========================================================"
Write-Host " overnet installer"
Write-Host "========================================================"

function Set-UserPath([bool]$add) {
    $p = [Environment]::GetEnvironmentVariable("Path", "User")
    $parts = @($p -split ";" | Where-Object { $_ -and $_ -ne $InstallDir })
    if ($add) { $parts += $InstallDir }
    [Environment]::SetEnvironmentVariable("Path", ($parts -join ";"), "User")
}

if ($Uninstall) {
    Stop-Process -Name "overnet" -Force -ErrorAction SilentlyContinue
    if (Test-Path $InstallDir) { Remove-Item $InstallDir -Recurse -Force }
    Set-UserPath $false
    Write-Host "overnet removed. Config and data ($DataDir) were kept; delete them by hand if you don't need them."
    return  # не exit: при запуске через iex он закрыл бы окно
}

# 1. Архитектура
$arch = "amd64"
if ($env:PROCESSOR_ARCHITECTURE -eq "ARM64" -or $env:PROCESSOR_ARCHITEW6432 -eq "ARM64") {
    # Сборки под ARM64 пока нет; x64 работает через эмуляцию Windows.
    Write-Host "[notice] ARM64 Windows: installing the x64 build (runs under emulation)."
}

# 2. Релиз
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
if ($Version) {
    $tag = if ($Version.StartsWith("v")) { $Version } else { "v$Version" }
} else {
    Write-Host "Fetching latest release..."
    try {
        $tag = (Invoke-RestMethod -Uri "https://api.github.com/repos/$repo/releases/latest" -UseBasicParsing).tag_name
    } catch {
        Write-Error "Could not determine the latest release (is GitHub reachable?). Pass -Version, or see https://github.com/$repo/releases"
        exit 1
    }
}

$archive = "overnet-windows-$arch.zip"
$url = "https://github.com/$repo/releases/download/$tag/$archive"
$tmp = Join-Path $env:TEMP "overnet_install_$PID"
New-Item -ItemType Directory -Path $tmp -Force | Out-Null
$zip = Join-Path $tmp $archive

Write-Host "Downloading: $archive ($tag)"
try {
    Invoke-WebRequest -Uri $url -OutFile $zip -UseBasicParsing
} catch {
    Write-Error "Download failed: $url"
    exit 1
}
$expected = ""
# Через файл: Windows PowerShell 5.1 отдаёт .Content для octet-stream массивом байтов.
$sumFile = "$zip.sha256"
try {
    Invoke-WebRequest -Uri "$url.sha256" -OutFile $sumFile -UseBasicParsing
    $expected = ((Get-Content $sumFile -Raw).Trim() -split "\s+")[0]
} catch { }
if ($expected) {
    if ($expected -ne (Get-FileHash $zip -Algorithm SHA256).Hash) { Write-Error "Checksum mismatch for $archive." }
    Write-Host "Checksum OK."
}

Expand-Archive -Path $zip -DestinationPath (Join-Path $tmp "x") -Force

# 3. Установка (работающий overnet.exe останавливаем: Windows не даёт заменить открытый файл)
New-Item -ItemType Directory -Path $InstallDir -Force | Out-Null
Stop-Process -Name "overnet" -Force -ErrorAction SilentlyContinue
Start-Sleep -Milliseconds 500
Get-ChildItem -Path (Join-Path $tmp "x") -File -Recurse | ForEach-Object {
    Copy-Item -Path $_.FullName -Destination (Join-Path $InstallDir $_.Name) -Force
}
Remove-Item $tmp -Recurse -Force
Set-UserPath $true
$env:Path = "$env:Path;$InstallDir"
Write-Host "Installed: $InstallDir\overnet.exe ($tag)"

# 4. Конфиг
New-Item -ItemType Directory -Path $DataDir -Force | Out-Null
$first = -not (Test-Path $ConfigFile)
if ($first) {
    if ($ConfigUrl) {
        Write-Host "Downloading network config: $ConfigUrl"
        Invoke-WebRequest -Uri $ConfigUrl -OutFile $ConfigFile -UseBasicParsing
    } else {
        Copy-Item (Join-Path $InstallDir "config.example.json") $ConfigFile
    }
    Write-Host "Config: $ConfigFile"
} elseif ($ConfigUrl) {
    Write-Host "[notice] $ConfigFile already exists; -ConfigUrl ignored."
}

Write-Host "--------------------------------------------------------"
if ($first -and -not $ConfigUrl) {
    Write-Host "Fill in $ConfigFile`: `"relays`" (pubkey@host:port of the network's relays)"
    Write-Host "and `"reserved`" (addresses of name.ov, search.ov, mail.ov, files.ov, source.ov)."
    Write-Host "Get both from whoever runs the network, or try it locally: overnet demo"
}
Write-Host "Open a new terminal, then: overnet browser    Help: overnet help"
Write-Host "--------------------------------------------------------"
