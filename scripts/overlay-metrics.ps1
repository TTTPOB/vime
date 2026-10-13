param([string]$Executable = 'target/release/vime.exe', [string]$Output = 'target/overlay-metrics.json')
$ErrorActionPreference = 'Stop'
function Measure-Client([string[]]$LaunchArguments) {
    $process = Start-Process -FilePath $Executable -ArgumentList $LaunchArguments -PassThru
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
        if (!$process.HasExited) {
            if ($LaunchArguments -contains '--overlay-demo') {
                Stop-Process -Id $process.Id -Force
            } else {
                Start-Process -FilePath $Executable -ArgumentList '--stop' -Wait
                if (!$process.WaitForExit(3000)) {
                    Stop-Process -Id $process.Id -Force
                    throw 'Idle client did not exit after --stop.'
                }
            }
        }
    }
}
$result = @{
    executable_bytes = (Get-Item $Executable).Length
    idle = (Measure-Client @('--no-clipboard', '--log-file', 'target/overlay-idle.log'))
    visible = (Measure-Client @('--overlay-demo', '--no-tray'))
    windowless = (Measure-Client @('--overlay-demo', '--no-tray', '--no-window'))
    environment = "$([Environment]::OSVersion.VersionString); $([Environment]::ProcessorCount) logical CPUs"
    workload = 'Idle: actual tray client waiting for a hotkey. Visible/windowless: demo excludes microphone and network.'
}
$result | ConvertTo-Json -Depth 4 | Set-Content -Encoding UTF8 $Output
Get-Content $Output
