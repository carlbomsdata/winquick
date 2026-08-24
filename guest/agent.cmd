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

rem If a capability volume is attached (PowerShell, and later others), put it on
rem PATH so the user can just say `pwsh`. Drive letters are not guaranteed, so
rem probe rather than hard-coding one.
set WQPS=
for %%d in (D E F G H I J K L M N O P) do (
  if not defined WQPS if exist %%d:\pwsh\pwsh.exe set WQPS=%%d:\pwsh
)
if defined WQPS set PATH=%WQPS%;%PATH%

>%WQ%\WQREADY.TXT echo 1
mountvol %WQ% /P >nul 2>&1

:wait
mountvol %WQ% %WQVOL% >nul 2>&1
if exist %WQ%\WQGO.TXT goto exec
mountvol %WQ% /P >nul 2>&1
goto wait

:exec
del %WQ%\WQGO.TXT >nul 2>&1
rem A child cmd.exe, not `call`: the workload must not be able to end the agent
rem with `exit`, and its errorlevel has to come back cleanly.
cmd /c %WQ%\WQCMD.CMD > %WQ%\WQOUT.TXT 2> %WQ%\WQERR.TXT
set WQRC=%errorlevel%
rem `echo %WQRC%>file` would parse as a stdin redirect. Redirect first instead.
>%WQ%\WQCODE.TXT echo %WQRC%
mountvol %WQ% /P >nul 2>&1
goto wait
