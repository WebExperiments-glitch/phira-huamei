@echo off
chcp 65001 >nul
cd /d "%~dp0"
set RUST_BACKTRACE=1
echo 正在启动 phira（日志写入 phira_run.log）...
target\release\phira-main.exe > phira_run.log 2>&1
echo.
echo 已退出，退出码 %errorlevel%
echo 日志：%~dp0phira_run.log
pause
