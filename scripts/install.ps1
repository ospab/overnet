# Установщик overnet для Windows.
#
#   irm https://raw.githubusercontent.com/ospab/overnet/master/scripts/install.ps1 | iex
#   & ([scriptblock]::Create((irm …/install.ps1))) -ConfigUrl https://…/config.json
#
# Права администратора не нужны: бинарник — в %LOCALAPPDATA%\Programs\overnet
# (добавляется в PATH пользователя), конфиг — в %LOCALAPPDATA%\overnet\config.json,
# где overnet ищет его без --config. overnet browser — в
# %LOCALAPPDATA%\Programs\overnet-browser (Browser\ — программа, Data\ — профиль),
# с ярлыком в меню «Пуск»; -NoBrowser — без него. Повторный запуск обновляет всё,
# профиль браузера остаётся.
param(
    [string]$Version = "",
    [string]$ConfigUrl = "",
    [switch]$NoBrowser,
    [switch]$Uninstall
)

$ErrorActionPreference = "Stop"
$ProgressPreference = "SilentlyContinue"
$repo = "ospab/overnet"
$InstallDir = Join-Path $env:LOCALAPPDATA "Programs\overnet"
$DataDir = if ($env:OVERNET_HOME) { $env:OVERNET_HOME } else { Join-Path $env:LOCALAPPDATA "overnet" }
$ConfigFile = Join-Path $DataDir "config.json"
$BrowserDir = Join-Path $env:LOCALAPPDATA "Programs\overnet-browser"
$Shortcut = Join-Path ([Environment]::GetFolderPath("Programs")) "overnet browser.lnk"

Write-Host "========================================================"
Write-Host " overnet installer"
Write-Host "========================================================"

# Скачать файл со строкой прогресса: [#####-----] 45%  98.1 / 217.0 MB  12.3 MB/s.
# Свой индикатор, а не Invoke-WebRequest: его прогресс в Windows PowerShell 5.1
# замедляет скачивание в разы. Ошибка (404 и т.п.) — исключение, как у него.
function Get-File([string]$Uri, [string]$OutFile) {
    Add-Type -AssemblyName System.Net.Http
    $client = New-Object System.Net.Http.HttpClient
    $client.Timeout = [TimeSpan]::FromMinutes(30)
    try {
        $resp = $client.GetAsync($Uri, [System.Net.Http.HttpCompletionOption]::ResponseHeadersRead).GetAwaiter().GetResult()
        [void]$resp.EnsureSuccessStatusCode()
        $total = $resp.Content.Headers.ContentLength
        $in = $resp.Content.ReadAsStreamAsync().GetAwaiter().GetResult()
        $out = [System.IO.File]::Create($OutFile)
        try {
            $buf = New-Object byte[] 262144
            $done = 0L
            $watch = [Diagnostics.Stopwatch]::StartNew()
            $last = -1000
            while (($n = $in.Read($buf, 0, $buf.Length)) -gt 0) {
                $out.Write($buf, 0, $n)
                $done += $n
                $ms = $watch.ElapsedMilliseconds
                if ($ms - $last -ge 200) { $last = $ms; Show-Progress $done $total $ms }
            }
            Show-Progress $done $total $watch.ElapsedMilliseconds
            Write-Host ""
        } finally {
            $out.Close()
            $in.Close()
        }
    } finally {
        $client.Dispose()
    }
}

function Show-Progress([long]$done, $total, [long]$ms) {
    $mb = $done / 1MB
    $speed = if ($ms -gt 0) { "{0,6:N1} MB/s" -f ($mb / ($ms / 1000.0)) } else { "" }
    if ($total -gt 0) {
        $frac = [Math]::Min(1.0, $done / [double]$total)
        $fill = [int][Math]::Floor($frac * 30)
        $bar = ("#" * $fill) + ("-" * (30 - $fill))
        $line = "  [{0}] {1,3}%  {2,6:N1} / {3:N1} MB  {4}" -f $bar, [int]($frac * 100), $mb, ($total / 1MB), $speed
    } else {
        $line = "  {0,6:N1} MB  {1}" -f $mb, $speed
    }
    Write-Host -NoNewline "`r$line"
}

# Распаковать zip. Не Expand-Archive: в Windows PowerShell 5.1 он падает на
# именах с «$» и при ошибке откатывает всю распаковку. Служебное NSIS ($PLUGINSDIR,
# *.nsis), попавшее в архивы v0.2.3, пропускаем; пути вне $Dest — тоже.
function Expand-Zip([string]$Zip, [string]$Dest) {
    Add-Type -AssemblyName System.IO.Compression.FileSystem
    $root = [System.IO.Path]::GetFullPath($Dest).TrimEnd('\') + '\'
    $z = [System.IO.Compression.ZipFile]::OpenRead($Zip)
    try {
        foreach ($e in $z.Entries) {
            $name = $e.FullName
            if ($name -match '(^|/)\$' -or $name.EndsWith('.nsis')) { continue }
            $path = [System.IO.Path]::GetFullPath((Join-Path $Dest ($name -replace '/', '\')))
            if (-not $path.StartsWith($root, [StringComparison]::OrdinalIgnoreCase)) { continue }
            if ($name.EndsWith('/')) {
                New-Item -ItemType Directory -Path $path -Force | Out-Null
                continue
            }
            New-Item -ItemType Directory -Path (Split-Path $path) -Force | Out-Null
            [System.IO.Compression.ZipFileExtensions]::ExtractToFile($e, $path, $true)
        }
    } finally {
        $z.Dispose()
    }
}

function Set-UserPath([bool]$add) {
    $p = [Environment]::GetEnvironmentVariable("Path", "User")
    $parts = @($p -split ";" | Where-Object { $_ -and $_ -ne $InstallDir })
    if ($add) { $parts += $InstallDir }
    [Environment]::SetEnvironmentVariable("Path", ($parts -join ";"), "User")
}

if ($Uninstall) {
    Stop-Process -Name "overnet-browser", "overnet" -Force -ErrorAction SilentlyContinue
    Start-Sleep -Milliseconds 500
    if (Test-Path $InstallDir) { Remove-Item $InstallDir -Recurse -Force }
    $browserBin = Join-Path $BrowserDir "Browser"
    if (Test-Path $browserBin) { Remove-Item $browserBin -Recurse -Force }
    Remove-Item $Shortcut -Force -ErrorAction SilentlyContinue
    Set-UserPath $false
    Write-Host "overnet removed. Config and data ($DataDir) and the browser profile ($BrowserDir\Data)"
    Write-Host "were kept; delete them by hand if you don't need them."
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
    Get-File -Uri $url -OutFile $zip
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
# Кроме `overnet update`, который запустил этот установщик: он ждёт его конца.
$updater = 0
[void][int]::TryParse("$env:OVERNET_UPDATER_PID", [ref]$updater)
Get-Process -Name "overnet" -ErrorAction SilentlyContinue | Where-Object { $_.Id -ne $updater } | Stop-Process -Force -ErrorAction SilentlyContinue
Start-Sleep -Milliseconds 500
Get-ChildItem -Path (Join-Path $tmp "x") -File -Recurse | ForEach-Object {
    Copy-Item -Path $_.FullName -Destination (Join-Path $InstallDir $_.Name) -Force
}
Remove-Item $tmp -Recurse -Force
Set-UserPath $true
$env:Path = "$env:Path;$InstallDir"
Write-Host "Installed: $InstallDir\overnet.exe ($tag)"

# 4. overnet browser
function Install-Browser {
    $name = "overnet-browser-windows-$arch.zip"
    $burl = "https://github.com/$repo/releases/download/$tag/$name"
    $btmp = Join-Path $env:TEMP "overnet_browser_$PID"
    New-Item -ItemType Directory -Path $btmp -Force | Out-Null
    $bzip = Join-Path $btmp $name
    Write-Host "Downloading: $name ($tag, about 220 MB)"
    try {
        Get-File -Uri $burl -OutFile $bzip
    } catch {
        Write-Host "[notice] $tag has no overnet browser build; skipping the browser."
        Remove-Item $btmp -Recurse -Force
        return
    }
    $want = ""
    try {
        Invoke-WebRequest -Uri "$burl.sha256" -OutFile "$bzip.sha256" -UseBasicParsing
        $want = ((Get-Content "$bzip.sha256" -Raw).Trim() -split "\s+")[0]
    } catch { }
    if ($want) {
        if ($want -ne (Get-FileHash $bzip -Algorithm SHA256).Hash) { Write-Error "Checksum mismatch for $name." }
        Write-Host "Checksum OK."
    }
    Stop-Process -Name "overnet-browser" -Force -ErrorAction SilentlyContinue
    Start-Sleep -Milliseconds 800
    # Заменяем только программу; профиль (Data\) остаётся.
    $bin = Join-Path $BrowserDir "Browser"
    if (Test-Path $bin) { Remove-Item $bin -Recurse -Force }
    New-Item -ItemType Directory -Path $BrowserDir -Force | Out-Null
    try {
        Expand-Zip $bzip $BrowserDir
    } finally {
        Remove-Item $btmp -Recurse -Force -ErrorAction SilentlyContinue
    }
    $exe = Join-Path $bin "overnet-browser.exe"
    $ws = New-Object -ComObject WScript.Shell
    $lnk = $ws.CreateShortcut($Shortcut)
    $lnk.TargetPath = $exe
    $lnk.WorkingDirectory = $bin
    $lnk.Description = "overnet browser"
    $lnk.Save()
    Write-Host "Installed: overnet browser ($exe), Start menu: overnet browser"
}
if (-not $NoBrowser) { Install-Browser }

# 5. Конфиг
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
Write-Host "The network's relays and service addresses are built in; $ConfigFile"
Write-Host "only needs changes for a network of your own."
if ($NoBrowser) {
    Write-Host "Open a new terminal, then: overnet browser    Help: overnet help"
} else {
    Write-Host "Start overnet browser from the Start menu (or: overnet browser)."
    Write-Host "Help: overnet help    Update later: overnet update"
}
Write-Host "--------------------------------------------------------"
