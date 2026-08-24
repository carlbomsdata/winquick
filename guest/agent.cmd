@echo off
rem WinQuick guest agent.
rem
rem Invoked through the cmd.exe AutoRun hook, so it runs as soon as the shell
rem starts. WQ_ACTIVE keeps it from re-entering when the workload spawns its own
rem cmd.exe.
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
if not exist %WQ%\WQCMD.CMD (
  echo [winquick] FATAL: no command in mailbox
  goto :eof
)
rem A child cmd.exe, not `call`: the workload must not be able to end the agent
rem with `exit`, and its errorlevel has to come back cleanly.
cmd /c %WQ%\WQCMD.CMD > %WQ%\WQOUT.TXT 2> %WQ%\WQERR.TXT
set RC=%errorlevel%
rem `echo %RC%>file` would parse as a stdin redirect. Redirect first instead.
>%WQ%\WQCODE.TXT echo %RC%
shutdown /s /t 0 /f
