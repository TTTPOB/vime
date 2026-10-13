param([Parameter(Mandatory = $true)][string]$Version)
$ErrorActionPreference = 'Stop'
$package = "vime-$Version-windows-x64"
$staging = Join-Path 'dist' $package
New-Item -ItemType Directory -Force $staging | Out-Null
Copy-Item 'target/release/vime.exe', 'target/release/vime-mock.exe' $staging
foreach ($file in @('README.md', 'LICENSE', 'vime.example.toml')) {
    Copy-Item $file $staging
}
Copy-Item 'docs' $staging -Recurse
$archive = Join-Path 'dist' "$package.zip"
Compress-Archive -Path $staging -DestinationPath $archive -Force
# Smoke-test the actual archived binaries after extracting the ZIP, using its
# own shipped configuration rather than a developer's local configuration.
$smokeDirectory = Join-Path 'dist' "$package-smoke"
Expand-Archive $archive -DestinationPath $smokeDirectory -Force
$extracted = (Resolve-Path (Join-Path $smokeDirectory $package)).Path
$executable = Join-Path $extracted 'vime.exe'
$stdout = Join-Path $extracted 'smoke.stdout'
$stderr = Join-Path $extracted 'smoke.stderr'
$process = Start-Process -FilePath $executable -WorkingDirectory $extracted -ArgumentList '--version' -Wait -PassThru -NoNewWindow -RedirectStandardOutput $stdout -RedirectStandardError $stderr
if ($process.ExitCode -ne 0) { throw "Archived executable --version failed: $(Get-Content $stderr -Raw)" }
$expectedVersion = $Version.TrimStart('v')
$actualVersion = (Get-Content $stdout -Raw).Trim()
if ($actualVersion -ne "vime $expectedVersion") { throw "Version mismatch: expected vime $expectedVersion; got $actualVersion" }
$process = Start-Process -FilePath $executable -WorkingDirectory $extracted -ArgumentList '--config', 'vime.example.toml', '--check-config', '--log-file', 'smoke.log' -Wait -PassThru -NoNewWindow -RedirectStandardOutput $stdout -RedirectStandardError $stderr
if ($process.ExitCode -ne 0) { throw "Archived executable --check-config failed: $(Get-Content $stderr -Raw)" }
if ((Get-Content $stdout -Raw).Trim() -ne 'configuration is valid') { throw 'Archived configuration smoke test did not produce its success message.' }
Write-Host 'PASS: extracted release ZIP executable version and shipped configuration.'
$hash = (Get-FileHash $archive -Algorithm SHA256).Hash.ToLowerInvariant()
"$hash  $package.zip" | Set-Content "$archive.sha256" -Encoding ascii
Remove-Item $staging, $smokeDirectory -Recurse -Force
