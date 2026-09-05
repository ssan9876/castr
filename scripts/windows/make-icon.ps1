# Draws castr's application icon and packs it into a multi-resolution .ico.
#
# The mark is a screen with arcs radiating from one corner - the visual
# language people already read as "casting". It has one idea and no detail,
# because it has to survive being drawn 16 pixels wide in a taskbar.
#
# The .ico this produces is committed, so an ordinary build never runs this.
# Re-run it only to change the artwork:
#   powershell -File scripts\windows\make-icon.ps1
param([string]$Out = "$PSScriptRoot\..\..\assets\castr.ico")

Add-Type -AssemblyName System.Drawing
$ErrorActionPreference = 'Stop'

$accent = [System.Drawing.Color]::FromArgb(255, 30, 111, 217)
$sizes  = 16, 24, 32, 48, 64, 128, 256

function New-Frame([int]$s) {
    $bmp = New-Object System.Drawing.Bitmap $s, $s,
        ([System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.Clear([System.Drawing.Color]::Transparent)

    # The screen: a rounded rectangle occupying most of the canvas.
    $x = [float]($s * 0.08); $y = [float]($s * 0.14)
    $w = [float]($s * 0.84); $h = [float]($s * 0.66)
    $r = [float]([Math]::Max(2.0, $s * 0.12))
    $path = New-Object System.Drawing.Drawing2D.GraphicsPath
    $path.AddArc($x, $y, $r, $r, 180, 90)
    $path.AddArc($x + $w - $r, $y, $r, $r, 270, 90)
    $path.AddArc($x + $w - $r, $y + $h - $r, $r, $r, 0, 90)
    $path.AddArc($x, $y + $h - $r, $r, $r, 90, 90)
    $path.CloseFigure()
    $brush = New-Object System.Drawing.SolidBrush $accent
    $g.FillPath($brush, $path)

    # The arcs, knocked out in white from the lower-left of the screen.
    $ox = [float]($x + $s * 0.16)
    $oy = [float]($y + $h - $s * 0.14)
    $pen = New-Object System.Drawing.Pen ([System.Drawing.Color]::White),
        ([float]([Math]::Max(1.3, $s * 0.062)))
    $pen.StartCap = [System.Drawing.Drawing2D.LineCap]::Round
    $pen.EndCap = [System.Drawing.Drawing2D.LineCap]::Round
    # Radii chosen so the outermost arc stays inside the screen with margin.
    # Larger ones ran off the canvas, which read as clipping rather than design.
    foreach ($f in 0.15, 0.27, 0.39) {
        $rad = [float]($s * $f)
        $g.DrawArc($pen, ($ox - $rad), ($oy - $rad), ($rad * 2), ($rad * 2), -90, 90)
    }
    # The origin dot, which is what makes it read as a source rather than a bowl.
    $dot = [float]([Math]::Max(1.6, $s * 0.085))
    $white = New-Object System.Drawing.SolidBrush ([System.Drawing.Color]::White)
    $g.FillEllipse($white, ($ox - $dot / 2), ($oy - $dot / 2), $dot, $dot)

    $brush.Dispose(); $white.Dispose(); $pen.Dispose(); $path.Dispose(); $g.Dispose()
    $bmp
}

# Each frame is stored as PNG, which Windows has accepted inside an .ico since
# Vista and which keeps the 256x256 frame from dominating the file size.
$frames = @()
foreach ($s in $sizes) {
    $bmp = New-Frame $s
    $ms = New-Object System.IO.MemoryStream
    $bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
    $frames += , @{ size = $s; bytes = $ms.ToArray() }
    $ms.Dispose(); $bmp.Dispose()
}

$dir = Split-Path -Parent $Out
if (-not (Test-Path $dir)) { New-Item -ItemType Directory -Path $dir | Out-Null }

$fs = [System.IO.File]::Create($Out)
$bw = New-Object System.IO.BinaryWriter $fs
$bw.Write([uint16]0)                 # reserved
$bw.Write([uint16]1)                 # type: icon
$bw.Write([uint16]$frames.Count)
# Directory entries come first, so every image offset is known up front.
$offset = 6 + 16 * $frames.Count
foreach ($f in $frames) {
    $dim = if ($f.size -ge 256) { 0 } else { $f.size }   # 0 means 256
    $bw.Write([byte]$dim); $bw.Write([byte]$dim)
    $bw.Write([byte]0)                                    # palette size
    $bw.Write([byte]0)                                    # reserved
    $bw.Write([uint16]1)                                  # colour planes
    $bw.Write([uint16]32)                                 # bits per pixel
    $bw.Write([uint32]$f.bytes.Length)
    $bw.Write([uint32]$offset)
    $offset += $f.bytes.Length
}
foreach ($f in $frames) { $bw.Write($f.bytes) }
$bw.Flush(); $bw.Dispose(); $fs.Dispose()

"wrote $Out"
"frames: $($sizes -join ', ')"
"bytes : $((Get-Item $Out).Length)"
