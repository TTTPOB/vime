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
$hash = (Get-FileHash $archive -Algorithm SHA256).Hash.ToLowerInvariant()
"$hash  $package.zip" | Set-Content "$archive.sha256" -Encoding ascii
Remove-Item $staging -Recurse -Force
