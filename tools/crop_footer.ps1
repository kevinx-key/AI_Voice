$ErrorActionPreference = "Continue"
Add-Type -AssemblyName System.Drawing
$src = "K:\AI_Voice_rs\.cache\gui_shot.png"
$img = [System.Drawing.Image]::FromFile($src)
$w = $img.Width; $h = $img.Height
$top = [int]($h * 0.82)
$bh = $h - $top
$bmp = New-Object System.Drawing.Bitmap $w, $bh
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.DrawImage($img, (New-Object System.Drawing.Rectangle 0, 0, $w, $bh), (New-Object System.Drawing.Rectangle 0, $top, $w, $bh), [System.Drawing.GraphicsUnit]::Pixel)
# 放大 2 倍便于识别
$big = New-Object System.Drawing.Bitmap ($w * 2), ($bh * 2)
$g2 = [System.Drawing.Graphics]::FromImage($big)
$g2.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::NearestNeighbor
$g2.DrawImage($bmp, 0, 0, $w * 2, $bh * 2)
$out = "C:\Users\KEVIN-~1\AppData\Local\Temp\opencode\gui_footer.png"
$big.Save($out, [System.Drawing.Imaging.ImageFormat]::Png)
$g.Dispose(); $g2.Dispose(); $bmp.Dispose(); $big.Dispose(); $img.Dispose()
Write-Output "saved $out  $($w*2)x$($bh*2)  (source region y=$top..$h)"
