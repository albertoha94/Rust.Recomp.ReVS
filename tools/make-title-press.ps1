# Renders tools/revs-run/assets/title_press.png: "Press any key", which the port shows on the title
# screen in place of the game's "Tap" (textures.rs puts it on the title sheet, lyt.rs points the "Tap"
# part at it). 4x the game's size, at most 1016 x 192 (254 x 48 points): a mint serif (Palatino
# Linotype) with wide letter spacing and a soft teal glow, like the game's "Tap".
param([string]$Text = "Press any key")
Add-Type -AssemblyName System.Drawing
$out = Join-Path $PSScriptRoot "revs-run\assets\title_press.png"
New-Item -ItemType Directory -Force (Split-Path $out) | Out-Null
$h = 192; $em = 112.0; $baseline = 126.0; $track = 0.09 * $em; $glow = 26
$family = New-Object System.Drawing.FontFamily "Palatino Linotype"
$style = [int][System.Drawing.FontStyle]::Regular
$ascent = $em * $family.GetCellAscent($style) / $family.GetEmHeight($style)
# One letter at a time, for the wide spacing.
$path = New-Object System.Drawing.Drawing2D.GraphicsPath
$probe = New-Object System.Drawing.Bitmap 1, 1
$pg = [System.Drawing.Graphics]::FromImage($probe)
$font = New-Object System.Drawing.Font $family, $em, ([System.Drawing.FontStyle]::Regular), ([System.Drawing.GraphicsUnit]::Pixel)
$fmt = [System.Drawing.StringFormat]::GenericTypographic
$fmt.FormatFlags = $fmt.FormatFlags -bor [System.Drawing.StringFormatFlags]::MeasureTrailingSpaces
$x = [double]$glow + 4
foreach ($c in $Text.ToCharArray()) {
    $s = [string]$c
    $path.AddString($s, $family, $style, $em, (New-Object System.Drawing.PointF $x, ($baseline - $ascent)), $fmt)
    $x += $pg.MeasureString($s, $font, 4000, $fmt).Width + $track
}
$pg.Dispose(); $probe.Dispose()
$b = $path.GetBounds()
# Centred in a width that is a multiple of 4 (the port box-filters it 4:1 for a 1x sheet).
$w = [int](4 * [math]::Ceiling(($b.Width + 2 * $glow + 8) / 4))
$m = New-Object System.Drawing.Drawing2D.Matrix
$m.Translate(($w - $b.Width) / 2 - $b.X, 0)
$path.Transform($m)
$bmp = New-Object System.Drawing.Bitmap $w, $h, ([System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
$g = [System.Drawing.Graphics]::FromImage($bmp)
$g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
$g.Clear([System.Drawing.Color]::Transparent)
# The glow: wide, faint teal strokes, narrowing and brightening towards the letters.
for ($i = 0; $i -lt 9; $i++) {
    $pw = $glow * 2 * (1 - $i / 9.0)
    $pen = New-Object System.Drawing.Pen ([System.Drawing.Color]::FromArgb(15, 50, 175, 150)), $pw
    $pen.LineJoin = [System.Drawing.Drawing2D.LineJoin]::Round
    $g.DrawPath($pen, $path)
    $pen.Dispose()
}
# A thin dark edge, then the mint letters, lighter at the top.
$edge = New-Object System.Drawing.Pen ([System.Drawing.Color]::FromArgb(150, 10, 45, 40)), 4
$edge.LineJoin = [System.Drawing.Drawing2D.LineJoin]::Round
$g.DrawPath($edge, $path)
$top = $baseline - 0.72 * $em
$fill = New-Object System.Drawing.Drawing2D.LinearGradientBrush (New-Object System.Drawing.PointF 0, $top), (New-Object System.Drawing.PointF 0, $baseline), ([System.Drawing.Color]::FromArgb(255, 145, 245, 220)), ([System.Drawing.Color]::FromArgb(255, 88, 198, 170))
$g.FillPath($fill, $path)
$g.Dispose()
$bmp.Save($out, [System.Drawing.Imaging.ImageFormat]::Png)
Write-Output "$out ($w x $h)"
