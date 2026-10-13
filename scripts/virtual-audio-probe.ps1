$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'
$download = Join-Path $env:RUNNER_TEMP 'VBCABLE_Driver_Pack45.zip'
$directory = Join-Path $env:RUNNER_TEMP 'vbcable-official-pack45'
$expectedHash = 'b950e39f01af1d04ea623c8f6d8eb9b6ea5c477c637295fabf20631c85116bfb'
Invoke-WebRequest 'https://download.vb-audio.com/Download_CABLE/VBCABLE_Driver_Pack45.zip' -OutFile $download
$actualHash = (Get-FileHash $download -Algorithm SHA256).Hash.ToLowerInvariant()
if ($actualHash -ne $expectedHash) { throw 'Official driver download checksum mismatch; refusing installation.' }
Expand-Archive $download -DestinationPath $directory
$installer = Join-Path $directory 'VBCABLE_Setup_x64.exe'
$catalog = Join-Path $directory 'vbaudio_cable64_win10.cat'
foreach ($signedFile in @($installer, $catalog)) {
    $signature = Get-AuthenticodeSignature $signedFile
    Write-Host "Signature for $signedFile : $($signature.Status), $($signature.SignerCertificate.Subject)"
    if ($signature.Status -ne 'Valid') { throw "UNSUPPORTED: Windows does not trust signature for $signedFile; refusing installation." }
    $expectedSubject = if ($signedFile -eq $installer) { 'CN=BUREL VINCENT Entrepreneur individuel' } else { 'CN=Microsoft Windows Hardware Compatibility Publisher' }
    if (-not $signature.SignerCertificate.Subject.Contains($expectedSubject)) { throw 'Unexpected signing identity; refusing installation.' }
}
# Use only the vendor's original signed installer; do not import certificates,
# change driver signature enforcement, or distribute the driver with vime.
foreach ($service in @('AudioEndpointBuilder', 'Audiosrv')) {
    if ((Get-Service $service).Status -ne 'Running') { Start-Service $service }
}
$process = Start-Process -FilePath $installer -WorkingDirectory $directory -ArgumentList '-i', '-h' -PassThru
$deadline = [DateTime]::UtcNow.AddSeconds(90)
while (-not $process.WaitForExit(1000)) {
    if ([DateTime]::UtcNow -ge $deadline) {
        taskkill.exe /PID $process.Id /T /F
        throw 'UNSUPPORTED: signed installer timed out; no WASAPI verification performed.'
    }
}
Write-Host "Vendor installer exit code: $($process.ExitCode)"
if ($process.ExitCode -notin @(0, 3010)) { throw 'UNSUPPORTED: vendor installer failed; see runner logs.' }
# Enumeration, not installer exit status, determines whether a rebootless probe
# is possible. The Rust test further requires actual signal through WASAPI.
$deadline = [DateTime]::UtcNow.AddSeconds(30)
do {
    $endpoints = @(Get-PnpDevice -Class AudioEndpoint -ErrorAction SilentlyContinue | Where-Object { $_.FriendlyName -like '*CABLE*' -and $_.Status -eq 'OK' })
    if ($endpoints.Count -ge 2) { break }
    Start-Sleep -Seconds 1
} while ([DateTime]::UtcNow -lt $deadline)
$endpoints | Format-Table Status, FriendlyName, InstanceId
if ($endpoints.Count -lt 2) { throw 'UNSUPPORTED: no usable virtual cable endpoints without reboot; real WASAPI capture remains unverified.' }
cargo test --locked --test windows_audio virtual_cable_wasapi_signal -- --ignored --nocapture
if ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }
