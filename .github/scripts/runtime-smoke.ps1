param(
  [ValidateRange(1, 60)][int]$MeasureSeconds = 5,
  [ValidateRange(1, 100)][double]$MaxCpuPercent = 25,
  [ValidateRange(1, 2048)][int]$MaxWorkingSetMb = 384,
  [ValidateRange(1, 2048)][int]$MaxPrivateMemoryMb = 384,
  [ValidateRange(1, 10000)][int]$MaxHandles = 1200,
  [ValidateRange(1, 1000)][int]$MaxThreads = 100
)

$ErrorActionPreference = "Stop"
$Root = (Resolve-Path -LiteralPath (Join-Path $PSScriptRoot "..\..")).Path
$Exe = Join-Path $Root "src-tauri\target\debug\agent-juice.exe"
$DataDir = Join-Path ([IO.Path]::GetTempPath()) ("agent-juice-ci-smoke-" + [guid]::NewGuid().ToString("N"))
$Process = $null

try {
  if (!(Test-Path -LiteralPath $Exe)) { throw "Debug app is missing: $Exe" }
  New-Item -ItemType Directory -Path $DataDir | Out-Null
  [IO.File]::WriteAllText(
    (Join-Path $DataDir "settings.json"),
    '{"show_claude":false,"show_codex":false,"update_check_on":false,"autostart_on":false}',
    [Text.UTF8Encoding]::new($false)
  )
  $env:AGENT_JUICE_DATA_DIR = $DataDir
  $Process = Start-Process -FilePath $Exe -WorkingDirectory $Root -WindowStyle Hidden -PassThru
  Start-Sleep -Seconds 3
  if ($Process.HasExited) { throw "App exited during smoke startup." }

  $Process.Refresh()
  $cpuStart = $Process.CPU
  [long]$maxWorkingSet = $Process.WorkingSet64
  [long]$maxPrivateMemory = $Process.PrivateMemorySize64
  $maxHandlesSeen = $Process.HandleCount
  $maxThreadsSeen = $Process.Threads.Count
  $watch = [Diagnostics.Stopwatch]::StartNew()
  while ($watch.Elapsed.TotalSeconds -lt $MeasureSeconds) {
    Start-Sleep -Milliseconds 500
    if ($Process.HasExited) { throw "App exited during smoke measurement." }
    $Process.Refresh()
    $maxWorkingSet = [Math]::Max($maxWorkingSet, $Process.WorkingSet64)
    $maxPrivateMemory = [Math]::Max($maxPrivateMemory, $Process.PrivateMemorySize64)
    $maxHandlesSeen = [Math]::Max($maxHandlesSeen, $Process.HandleCount)
    $maxThreadsSeen = [Math]::Max($maxThreadsSeen, $Process.Threads.Count)
  }
  $watch.Stop()
  $Process.Refresh()
  $cpuPercent = 100.0 * ($Process.CPU - $cpuStart) / $watch.Elapsed.TotalSeconds
  if ($cpuPercent -gt $MaxCpuPercent) { throw "CPU $cpuPercent% exceeded $MaxCpuPercent%." }
  if (($maxWorkingSet / 1MB) -gt $MaxWorkingSetMb) { throw "Working set exceeded $MaxWorkingSetMb MiB." }
  if (($maxPrivateMemory / 1MB) -gt $MaxPrivateMemoryMb) { throw "Private memory exceeded $MaxPrivateMemoryMb MiB." }
  if ($maxHandlesSeen -gt $MaxHandles) { throw "Handle count exceeded $MaxHandles." }
  if ($maxThreadsSeen -gt $MaxThreads) { throw "Thread count exceeded $MaxThreads." }

  [pscustomobject]@{
    cpu_percent_one_core = [Math]::Round($cpuPercent, 3)
    working_set_high_water_mb = [Math]::Round($maxWorkingSet / 1MB, 3)
    private_memory_high_water_mb = [Math]::Round($maxPrivateMemory / 1MB, 3)
    handles_high_water = $maxHandlesSeen
    threads_high_water = $maxThreadsSeen
  } | ConvertTo-Json -Compress
}
finally {
  if ($null -ne $Process -and !$Process.HasExited) {
    Stop-Process -Id $Process.Id -Force -ErrorAction SilentlyContinue
    Wait-Process -Id $Process.Id -Timeout 5 -ErrorAction SilentlyContinue
  }
  Remove-Item Env:AGENT_JUICE_DATA_DIR -ErrorAction SilentlyContinue
  $resolved = [IO.Path]::GetFullPath($DataDir)
  $tempRoot = [IO.Path]::GetFullPath([IO.Path]::GetTempPath())
  if ($resolved.StartsWith($tempRoot, [StringComparison]::OrdinalIgnoreCase) -and
      (Split-Path $resolved -Leaf).StartsWith("agent-juice-ci-smoke-")) {
    Remove-Item -LiteralPath $resolved -Recurse -Force -ErrorAction SilentlyContinue
  }
}
