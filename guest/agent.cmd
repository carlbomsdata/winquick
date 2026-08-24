@echo off
rem WinQuick guest agent -- mailbox protocol v1.
rem
rem Started through the cmd.exe AutoRun hook, so it runs as soon as the shell
rem does. WQ_ACTIVE stops it re-entering when the workload spawns its own
rem cmd.exe.
rem
rem The agent never shuts the machine down. It announces readiness, then waits
rem for work. The host decides when the VM dies. That is what lets a booted
rem guest be captured once and restored for every later run.
if defined WQ_ACTIVE goto :eof
set WQ_ACTIVE=1

set WQ=
for %%d in (D E F G H I J K L M N O P Q R S T U V W X Y Z) do (
  if not defined WQ if exist %%d:\WQMARK.TXT set WQ=%%d:
)
if not defined WQ (
  echo [winquick] FATAL: mailbox volume not found
  goto :eof
)

rem Windows only synchronises a FAT volume with the underlying disk at mount and
rem dismount. Without this the host would never see our writes and we would never
rem see the host's. The volume GUID is stable for the life of the filesystem, so
rem stash it and use it to re-create the mount point on demand.
for /f "tokens=*" %%v in ('mountvol %WQ% /L') do set WQVOL=%%v

rem Capability volumes (PowerShell, .NET) are attached as extra disks. Drive
rem letters are not guaranteed, so probe for known layouts rather than assuming.
set WQPS=
set WQDOTNET=
for %%d in (D E F G H I J K L M N O P) do (
  if not defined WQPS if exist %%d:\pwsh\pwsh.exe set WQPS=%%d:\pwsh
  if not defined WQDOTNET if exist %%d:\dotnet\dotnet.exe set WQDOTNET=%%d:\dotnet
)
if defined WQPS set PATH=%WQPS%;%PATH%
if defined WQDOTNET (
  set PATH=%WQDOTNET%;%PATH%
  set DOTNET_ROOT=%WQDOTNET%
  rem Keep the CLI quiet and self-contained: no telemetry, no first-run banner,
  rem and a writable home on C: rather than wherever it would otherwise guess.
  set DOTNET_CLI_TELEMETRY_OPTOUT=1
  set DOTNET_NOLOGO=1
  set DOTNET_SKIP_FIRST_TIME_EXPERIENCE=1
  set DOTNET_CLI_HOME=C:\dotnet-home
  if not exist C:\dotnet-home mkdir C:\dotnet-home
)

rem The workspace, artifact and package-cache volumes all change contents between
rem runs, so remember their identities now and re-read them just before executing.
set WQWS=
set WQWSVOL=
set WQART=
set WQARTVOL=
set WQNUGET=
set WQNUGETVOL=
for %%d in (D E F G H I J K L M N O P) do (
  if not defined WQWS if exist %%d:\WQWORK.TXT set WQWS=%%d:
  if not defined WQART if exist %%d:\WQARTS.TXT set WQART=%%d:
  if not defined WQNUGET if exist %%d:\WQNUGET.TXT set WQNUGET=%%d:
)
if defined WQWS for /f "tokens=*" %%v in ('mountvol %WQWS% /L') do set WQWSVOL=%%v
if defined WQART for /f "tokens=*" %%v in ('mountvol %WQART% /L') do set WQARTVOL=%%v
if defined WQNUGET for /f "tokens=*" %%v in ('mountvol %WQNUGET% /L') do set WQNUGETVOL=%%v
rem Packages come from the host-managed cache. The guest gets a throwaway clone of
rem it, so anything a build writes here is discarded with the rest of the run.
if defined WQNUGET set NUGET_PACKAGES=%WQNUGET%\packages

>%WQ%\WQREADY.TXT echo 1
mountvol %WQ% /P >nul 2>&1

:wait
mountvol %WQ% %WQVOL% >nul 2>&1
if exist %WQ%\WQGO.TXT goto exec
mountvol %WQ% /P >nul 2>&1
goto wait

:exec
del %WQ%\WQGO.TXT >nul 2>&1
rem Same cache problem as the mailbox: the guest is holding a stale view of the
rem workspace from before it was frozen. Dismount and remount to see this run's
rem files, then surface them at a predictable path.
if defined WQWSVOL (
  mountvol %WQWS% /P >nul 2>&1
  mountvol %WQWS% %WQWSVOL% >nul 2>&1
  if exist %WQWS%\workspace (
    if not exist C:\workspace mklink /J C:\workspace %WQWS%\workspace >nul 2>&1
    cd /d C:\workspace >nul 2>&1
  )
)
rem Echoed back with the exit code so the host can prove this run actually read
rem this run's command. If the guest is holding a stale view of the mailbox it
rem would otherwise run an empty batch and report a confident, wrong success.
set WQNONCE=
if exist %WQ%\WQNONCE.TXT set /p WQNONCE=<%WQ%\WQNONCE.TXT
rem A child cmd.exe, not `call`: the workload must not be able to end the agent
rem with `exit`, and its errorlevel has to come back cleanly.
cmd /c %WQ%\WQCMD.CMD > %WQ%\WQOUT.TXT 2> %WQ%\WQERR.TXT
set WQRC=%errorlevel%
rem Artifacts are collected even when the command failed - a failed build's logs
rem are usually the thing you wanted. The command's exit code is already saved.
if defined WQARTVOL if exist %WQ%\WQART.CMD (
  call %WQ%\WQART.CMD > %WQART%\WQARTLOG.TXT 2>&1
  mountvol %WQART% /P >nul 2>&1
)
rem `echo %WQRC%>file` would parse as a stdin redirect. Redirect first instead.
>%WQ%\WQCODE.TXT echo %WQRC% %WQNONCE%
mountvol %WQ% /P >nul 2>&1
goto wait
