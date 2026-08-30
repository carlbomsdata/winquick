import base64, sys
cpus, label, cmd, timeout, cold, allowcold = sys.argv[1:7]
ps = f'''
$root  = Join-Path $env:USERPROFILE ".winquick"
$note  = Join-Path $root "restore-unsupported"
$qemu  = "C:\\winquick-lab\\qemu-src\\build\\qemu-system-x86_64.exe"
$qi = if (Test-Path $qemu) {{ $f=Get-Item $qemu; "{{0}}@{{1}}" -f $f.Length,$f.LastWriteTimeUtc.Ticks }} else {{ "missing" }}
$noteTxt = if (Test-Path $note) {{ (Get-Content $note -Raw).Trim() }} else {{ "" }}
$stateDir = Join-Path $root "states\\validation-x64"
$sf = Join-Path $stateDir "ready.state"
$stHash = if (Test-Path $sf) {{ (Get-FileHash $sf -Algorithm SHA256).Hash.Substring(0,16) }} else {{ "none" }}
if ($noteTxt -and "{cold}" -ne "yes") {{
  Write-Output "PRECONDITION-FAIL restore-unsupported=$noteTxt"
  exit 3
}}
$extra = ""
if ("{cold}" -eq "yes") {{ $extra = "--cold" }}
$tmp = [IO.Path]::GetTempFileName()
$line = "run --cpus {cpus} --verbose --timeout {timeout} $extra -- {cmd}"
$sw = [Diagnostics.Stopwatch]::StartNew()
$p = Start-Process -FilePath "cmd.exe" -ArgumentList "/c C:\\wq\\wq.cmd $line > `"$tmp`" 2>&1" -NoNewWindow -Wait -PassThru
$rc = $p.ExitCode
$sw.Stop()
$out = (Get-Content $tmp -Raw -ErrorAction SilentlyContinue)
Remove-Item $tmp -Force -ErrorAction SilentlyContinue
if (-not $out) {{ $out = "" }}
$warm = $out -match "warm run, total"
$cold2 = $out -match "cold run, total"
$attempted = $out -match "using existing ready state|another run prepared"
$fellback = $out -match "warm path failed|discarding ready state|do not restore with this QEMU"
$built = $out -match "ready state built"
$class = if ($warm) {{ "WARM" }} elseif ($cold2) {{ "COLD" }} else {{ "UNKNOWN" }}
Write-Output ("label={label} cpus={cpus} class=$class rc=$rc elapsed=" + [int]$sw.Elapsed.TotalMilliseconds + "ms")
Write-Output ("  qemu=$qi state=$stHash attempted=$attempted fellback=$fellback built=$built")
foreach ($l in ($out -split "`r?`n")) {{
  if ($l -and $l -notmatch "^winquick: C:" -and $l -notmatch "^winquick: .*qemu-system") {{ Write-Output ("  | " + $l) }}
}}
if ("{cold}" -ne "yes" -and "{allowcold}" -ne "yes" -and $class -ne "WARM") {{
  Write-Output "EXPERIMENT-INVALID wanted=WARM got=$class"
  exit 4
}}
exit 0
'''
sys.stdout.write(base64.b64encode(ps.encode('utf-16-le')).decode())
