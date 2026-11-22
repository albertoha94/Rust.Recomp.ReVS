# Renders tools/revs-run/assets/title_credit.png: the text the port adds after the title screen's
# "(c) CAPCOM 2011" (textures.rs). 4x the game's size (60 px high for a 15-point line): light grey Arial
# with a dark outline, like the original line. Only this text is stored; the game's own line is copied
# from the user's game at run time.
param([string]$Text = "  " + [char]0x00B7 + "  Recomp by albertoha94")
Add-Type -AssemblyName System.Drawing
$out = Join-Path $PSScriptRoot "revs-run\assets\title_credit.png"
New-Item -ItemType Directory -Force (Split-Path $out) | Out-Null
$h = 60; $em = 50.0; $baseline = 46.0
$font = New-Object System.Drawing.FontFamily "Arial"
$path = New-Object System.Drawing.Drawing2D.GraphicsPath
# AddString places the em box top at y; Arial's ascent is 0.905 em.
$path.AddString($Text, $font, [int][System.Drawing.FontStyle]::Regular, $em, (New-Object System.Drawing.PointF 4, ($baseline - 0.905 * $em)), [System.Drawing.StringFormat]::GenericTypographic)
$b = $path.GetBounds()
$w = [int][math]::Ceiling($b.Right + 8)
$bmp = New-Object System.Drawing.Bitmap $w, $h, ([System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
$g.Clear([System.Drawing.Color]::Transparent)
$pen = New-Object System.Drawing.Pen ([System.Drawing.Color]::FromArgb(220, 10, 10, 10)), 8
$pen.LineJoin = [System.Drawing.Drawing2D.LineJoin]::Round
$g.DrawPath($pen, $path)
$g.FillPath((New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::FromArgb(255, 230, 230, 230))), $path)
$g.Dispose()
$bmp.Save($out, [System.Drawing.Imaging.ImageFormat]::Png)
Write-Output "$out ($w x $h)"
