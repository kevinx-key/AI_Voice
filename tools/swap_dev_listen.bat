@echo off
chcp 936 >nul
setlocal
echo ============================================================
echo   倾听输入法 - 开发版 listen.exe 替换脚本 (任务2 托管版)
echo   来源: K:\AI_Voice_rs\target\release\listen.exe
echo   目标: K:\app\AIVoice\listen.exe
echo   说明: 替换后不要手动启动 listen, 由倾听输入法(fork)激活时拉起
echo ============================================================
echo.
set SRC=K:\AI_Voice_rs\target\release\listen.exe
set DST=K:\app\AIVoice\listen.exe
if not exist "%SRC%" (
  echo [错误] 找不到新版本: %SRC%
  pause
  exit /b 1
)
taskkill /IM listen.exe /F >nul 2>nul
timeout /t 2 /nobreak >nul
if not exist "%DST%.bak-20260928" copy /Y "%DST%" "%DST%.bak-20260928" >nul
copy /Y "%SRC%" "%DST%"
if errorlevel 1 (
  echo [错误] 复制失败, 请检查文件是否被占用
  pause
  exit /b 1
)
echo.
echo [完成] 已替换为开发版. 原版备份: %DST%.bak-20260928
echo 下一步: 切换到「倾听输入法」, 应自动拉起 listen --managed
echo.
pause
