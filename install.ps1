# Installs Carrick on Windows from the latest release, for the current user
# and without administrator rights. In PowerShell:
#
#   irm https://raw.githubusercontent.com/johan-socarras/Carrick/main/install.ps1 | iex
#
# It downloads carrick-<version>-setup.exe from GitHub, checks it against the
# SHA-256 GitHub publishes for it, and runs it silently. Running it again
# updates Carrick in place. Optional:
#
#   $env:CARRICK_VERSION = 'v0.1.7'   install that release instead of the latest
#   $env:GITHUB_TOKEN = '...'         for a private fork, or to avoid rate limits
#
# Works in Windows PowerShell 5.1 and PowerShell 7.

& {
    $ErrorActionPreference = 'Stop'
    $ProgressPreference = 'SilentlyContinue'   # Invoke-WebRequest is much faster without it
    $repo = 'johan-socarras/Carrick'

    function Stop-Install($message) {
        Write-Host $message -ForegroundColor Red
        throw 'Carrick was not installed.'
    }

    if ([Environment]::OSVersion.Platform -ne 'Win32NT') {
        Stop-Install 'This installer is for Windows. On Linux, build the .deb: see the README.'
    }
    # Windows PowerShell 5.1 may still offer only older TLS versions.
    [Net.ServicePointManager]::SecurityProtocol = [Net.ServicePointManager]::SecurityProtocol -bor [Net.SecurityProtocolType]::Tls12


    $headers = @{ 'User-Agent' = 'carrick-install'; 'Accept' = 'application/vnd.github+json' }
    if ($env:GITHUB_TOKEN) { $headers['Authorization'] = "Bearer $env:GITHUB_TOKEN" }
    $which = if ($env:CARRICK_VERSION) { "tags/$env:CARRICK_VERSION" } else { 'latest' }
    try {
        $release = Invoke-RestMethod -Uri "https://api.github.com/repos/$repo/releases/$which" -Headers $headers
    } catch {
        Stop-Install "Could not find the release '$which' of $repo on GitHub: $($_.Exception.Message)"
    }
    $asset = $release.assets | Where-Object { $_.name -match '^carrick-.+-setup\.exe$' } | Select-Object -First 1
    if (-not $asset) { Stop-Install "The release $($release.tag_name) has no Windows installer." }

    $running = Get-Process carrick, carrick-gui -ErrorAction SilentlyContinue
    if ($running) {
        Stop-Install 'Carrick is running. Close it (an unfinished transfer continues later, from where it was) and run this again.'
    }
    $folder = Join-Path ([IO.Path]::GetTempPath()) ("carrick-install-" + [Guid]::NewGuid().ToString('N'))
    New-Item -ItemType Directory $folder | Out-Null
    try {
        $setup = Join-Path $folder $asset.name
        Write-Host "Downloading $($asset.name) ($([Math]::Round($asset.size / 1MB, 1)) MB)..."
        if ($env:GITHUB_TOKEN) {
            # A private repository only serves its files through the API.
            $download = @{ 'User-Agent' = 'carrick-install'; 'Accept' = 'application/octet-stream'; 'Authorization' = "Bearer $env:GITHUB_TOKEN" }
            Invoke-WebRequest -Uri $asset.url -Headers $download -OutFile $setup -UseBasicParsing
        } else {
            Invoke-WebRequest -Uri $asset.browser_download_url -Headers @{ 'User-Agent' = 'carrick-install' } -OutFile $setup -UseBasicParsing
        }

        $size = (Get-Item $setup).Length
        if ($size -ne $asset.size) { Stop-Install "The download is incomplete: $size bytes of $($asset.size)." }
        if ($asset.digest -match '^sha256:([0-9a-f]{64})$') {
            $hash = (Get-FileHash $setup -Algorithm SHA256).Hash.ToLowerInvariant()
            if ($hash -ne $Matches[1]) { Stop-Install "The download does not match the SHA-256 GitHub publishes for it." }
            Write-Host 'Checked against the SHA-256 published on GitHub.'
        } else {
            Write-Host 'GitHub publishes no SHA-256 for this file; checked its size only.' -ForegroundColor Yellow
        }

        Write-Host "Installing Carrick $($release.tag_name) for $env:USERNAME..."
        $process = Start-Process $setup -ArgumentList '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', '/CURRENTUSER' -Wait -PassThru
        if ($process.ExitCode -ne 0) { Stop-Install "The installer stopped with code $($process.ExitCode)." }
    } finally {
        Remove-Item $folder -Recurse -Force -ErrorAction SilentlyContinue
    }

    $installed = Join-Path $env:LOCALAPPDATA 'Programs\Carrick'
    $version = & (Join-Path $installed 'carrick.exe') --version 2>$null | Select-Object -First 1
    Write-Host "$version is installed in $installed." -ForegroundColor Green
    Write-Host 'Open it from the Start menu, or run carrick in a new terminal.'
}
