$ErrorActionPreference = "Continue"
Add-Type -AssemblyName System.Drawing
$src = "K:\AI_Voice_rs\.cache\gui_shot.png"
$img = [System.Drawing.Image]::FromFile($src)
$w = $img.Width; $h = $img.Height
Write-Output "src ${w}x${h}"
# 下半部分
$top = [int]($h * 0.45)
$bh = $h - $top
$bmp = New-Object System.Drawing.Bitmap $w, $bh
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.DrawImage($img, (New-Object System.Drawing.Rectangle 0, 0, $w, $bh), (New-Object System.Drawing.Rectangle 0, $top, $w, $bh), [System.Drawing.GraphicsUnit]::Pixel)
$out = "C:\Users\KEVIN-~1\AppData\Local\Temp\opencode\gui_bottom.png"
$bmp.Save($out, [System.Drawing.Imaging.ImageFormat]::Png)
$g.Dispose(); $bmp.Dispose(); $img.Dispose()
Write-Output "saved $out  ${w}x${bh}"
