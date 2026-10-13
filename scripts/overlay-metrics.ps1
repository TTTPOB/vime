param([string]$Executable = 'target/release/vime.exe', [string]$Output = 'target/overlay-metrics.json')
$ErrorActionPreference = 'Stop'
function Measure-Demo([string[]]$DemoArguments) {
    $process = Start-Process -FilePath $Executable -ArgumentList $DemoArguments -PassThru
    try {
        Start-Sleep -Milliseconds 1500
        $process.Refresh()
        $cpuBefore = $process.TotalProcessorTime.TotalMilliseconds
        $samples = @()
        1..5 | ForEach-Object {
            Start-Sleep -Milliseconds 500
            $process.Refresh()
            $samples += $process.WorkingSet64
        }
        $cpuMilliseconds = $process.TotalProcessorTime.TotalMilliseconds - $cpuBefore
        return @{ working_set_mean_bytes = [math]::Round(($samples | Measure-Object -Average).Average); working_set_peak_bytes = ($samples | Measure-Object -Maximum).Maximum; cpu_percent_one_core = [math]::Round($cpuMilliseconds / 2500 * 100, 2); sample_seconds = 2.5 }
    } finally {
        if (!$process.HasExited) { Stop-Process -Id $process.Id -Force }
    }
}
$result = @{ executable_bytes = (Get-Item $Executable).Length; visible = (Measure-Demo @('--overlay-demo', '--no-tray')); windowless = (Measure-Demo @('--overlay-demo', '--no-tray', '--no-window')); environment = 'GitHub Actions Windows runner; demo excludes microphone/network backend' }
$result | ConvertTo-Json -Depth 4 | Set-Content -Encoding UTF8 $Output
Get-Content $Output
