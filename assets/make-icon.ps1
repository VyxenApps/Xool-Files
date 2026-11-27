# Genera assets/xool-files.ico: carpeta ámbar con un frente plegado en diagonal.
# Uso: powershell -NoProfile -ExecutionPolicy Bypass -File assets\make-icon.ps1

$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Drawing

function New-RoundRect([float]$x, [float]$y, [float]$w, [float]$h, [float]$r) {
    $path = New-Object System.Drawing.Drawing2D.GraphicsPath
    $d = [Math]::Max(0.5, $r * 2.0)
    $path.AddArc($x, $y, $d, $d, 180, 90)
    $path.AddArc($x + $w - $d, $y, $d, $d, 270, 90)
    $path.AddArc($x + $w - $d, $y + $h - $d, $d, $d, 0, 90)
    $path.AddArc($x, $y + $h - $d, $d, $d, 90, 90)
    $path.CloseFigure()
    return $path
}

function New-FolderBitmap([int]$s) {
    $bmp = New-Object System.Drawing.Bitmap $s, $s, ([System.Drawing.Imaging.PixelFormat]::Format32bppArgb)
    $g = [System.Drawing.Graphics]::FromImage($bmp)
    $g.SmoothingMode = [System.Drawing.Drawing2D.SmoothingMode]::AntiAlias
    $g.InterpolationMode = [System.Drawing.Drawing2D.InterpolationMode]::HighQualityBicubic
    $g.PixelOffsetMode = [System.Drawing.Drawing2D.PixelOffsetMode]::HighQuality
    $g.Clear([System.Drawing.Color]::Transparent)

    $scale = $s / 256.0
    # Reduce the transparent inset without changing the Windows icon slot.
    $g.TranslateTransform($s / 2.0, $s / 2.0)
    $g.ScaleTransform($scale * 1.17, $scale * 1.17)
    $g.TranslateTransform(-128.0, -128.0)

    # Raised tab with a curved transition into the back of the folder.
    $back = [Drawing.Drawing2D.GraphicsPath]::new()
    $back.AddLine(24, 183, 24, 62)
    $back.AddBezier(24, 62, 24, 49, 32, 40, 44, 40)
    $back.AddLine(44, 40, 91, 40)
    $back.AddBezier(91, 40, 107, 40, 106, 63, 124, 63)
    $back.AddLine(124, 63, 212, 63)
    $back.AddBezier(212, 63, 224, 63, 232, 72, 232, 85)
    $back.AddLine(232, 85, 232, 183)
    $back.CloseFigure()
    $backBrush = [Drawing.Drawing2D.LinearGradientBrush]::new(
        [Drawing.RectangleF]::new(24, 40, 208, 153),
        [Drawing.Color]::FromArgb(239, 167, 58), [Drawing.Color]::FromArgb(198, 99, 28), 105.0)
    $g.FillPath($backBrush, $back)

    $front = New-RoundRect 24 86 208 128 18
    $frontBrush = [Drawing.Drawing2D.LinearGradientBrush]::new(
        [Drawing.RectangleF]::new(24, 86, 208, 128),
        [Drawing.Color]::FromArgb(255, 212, 116), [Drawing.Color]::FromArgb(238, 135, 36), 90.0)
    $g.FillPath($frontBrush, $front)

    # Cut the fold into the face without changing the outer silhouette.
    $fold = [Drawing.Drawing2D.GraphicsPath]::new()
    $fold.AddLines([Drawing.PointF[]]@(
        [Drawing.PointF]::new(176, 86),
        [Drawing.PointF]::new(214, 86),
        [Drawing.PointF]::new(232, 104),
        [Drawing.PointF]::new(232, 196),
        [Drawing.PointF]::new(214, 214),
        [Drawing.PointF]::new(112, 214),
        [Drawing.PointF]::new(148, 149)
    ))
    $fold.CloseFigure()
    $foldBrush = [Drawing.Drawing2D.LinearGradientBrush]::new(
        [Drawing.RectangleF]::new(112, 86, 120, 128),
        [Drawing.Color]::FromArgb(248, 179, 70), [Drawing.Color]::FromArgb(217, 106, 30), 120.0)
    $g.FillPath($foldBrush, $fold)

    # A pale folded lip follows the top corners of the front.
    $lip = [Drawing.Drawing2D.GraphicsPath]::new()
    $lip.AddBezier(24, 106, 24, 95, 32, 86, 42, 86)
    $lip.AddLine(42, 86, 214, 86)
    $lip.AddBezier(214, 86, 224, 86, 232, 95, 232, 106)
    $lip.AddLine(232, 106, 24, 106)
    $lip.CloseFigure()
    $lipBrush = [Drawing.Drawing2D.LinearGradientBrush]::new(
        [Drawing.RectangleF]::new(24, 86, 208, 20),
        [Drawing.Color]::FromArgb(255, 232, 166), [Drawing.Color]::FromArgb(255, 205, 110), 90.0)
    $g.FillPath($lipBrush, $lip)

    # The short inset follows the direction of the fold.
    $inset = [Drawing.Pen]::new([Drawing.Color]::FromArgb(255, 235, 158), 10)
    $inset.StartCap = [Drawing.Drawing2D.LineCap]::Round
    $inset.EndCap = [Drawing.Drawing2D.LineCap]::Round
    $g.DrawLine($inset, 54, 126, 114, 126)

    foreach ($resource in @($back, $backBrush, $front, $frontBrush, $fold, $foldBrush, $lip, $lipBrush, $inset)) {
        $resource.Dispose()
    }
    $g.Dispose()
    return $bmp
}

$sizes = @(16, 20, 24, 32, 40, 48, 64, 128, 256)
$pngs = @()
foreach ($s in $sizes) {
    $bmp = New-FolderBitmap $s
    $ms = New-Object System.IO.MemoryStream
    $bmp.Save($ms, [System.Drawing.Imaging.ImageFormat]::Png)
    $pngs += ,$ms.ToArray()
    $ms.Dispose()
    $bmp.Dispose()
}

$outDir = Split-Path -Parent $MyInvocation.MyCommand.Path
$icoPath = Join-Path $outDir 'xool-files.ico'
$fs = [System.IO.File]::Create($icoPath)
$bw = New-Object System.IO.BinaryWriter($fs)
$bw.Write([UInt16]0)
$bw.Write([UInt16]1)
$bw.Write([UInt16]$sizes.Count)
$offset = 6 + (16 * $sizes.Count)
for ($i = 0; $i -lt $sizes.Count; $i++) {
    $dim = if ($sizes[$i] -ge 256) { 0 } else { $sizes[$i] }
    $bw.Write([byte]$dim)
    $bw.Write([byte]$dim)
    $bw.Write([byte]0)
    $bw.Write([byte]0)
    $bw.Write([UInt16]1)
    $bw.Write([UInt16]32)
    $bw.Write([UInt32]$pngs[$i].Length)
    $bw.Write([UInt32]$offset)
    $offset += $pngs[$i].Length
}
foreach ($png in $pngs) { $bw.Write($png) }
$bw.Flush()
$bw.Dispose()
$fs.Dispose()

# Vista previa del tamano grande.
$preview = New-FolderBitmap 256
$preview.Save((Join-Path $outDir 'xool-files-preview.png'), [System.Drawing.Imaging.ImageFormat]::Png)
$preview.Dispose()

"ico=$icoPath bytes=$((Get-Item -LiteralPath $icoPath).Length) tamanos=$($sizes -join ',')"
