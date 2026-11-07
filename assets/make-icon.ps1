# Renders vitals-icon.svg with Edge headless, then writes vitals.ico (Explorer, shortcuts),
# vitals.res (embedded in the exe by build.rs) and macos/vitals.icns (the .app bundle).
# Run it after editing the SVG.
$dir = $PSScriptRoot
$edge = 'C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe'
$sizes = 1024, 512, 256, 128, 64, 48, 32, 24, 16
$tmp = Join-Path $env:TEMP 'vitals-icon'
New-Item -ItemType Directory -Force $tmp | Out-Null
Copy-Item "$dir\vitals-icon.svg" "$tmp\icon.svg"
Set-Content "$tmp\icon.html" '<html><body style="margin:0;background:transparent"><img src="icon.svg" style="width:100vh;height:100vh;display:block"></body></html>'

$pngs = foreach ($s in $sizes) {
    $out = "$tmp\$s.png"
    Remove-Item $out -ErrorAction SilentlyContinue
    & $edge --headless --disable-gpu --hide-scrollbars --force-device-scale-factor=1 --default-background-color=00000000 `
        --user-data-dir="$tmp\profile" --window-size="$s,$s" --screenshot="$out" "file:///$($tmp -replace '\\','/')/icon.html" 2>$null | Out-Null
    for ($i = 0; $i -lt 40 -and -not (Test-Path $out); $i++) { Start-Sleep -Milliseconds 250 }
    [pscustomobject]@{ Size = $s; Bytes = [IO.File]::ReadAllBytes($out) }
}
Copy-Item "$tmp\64.png" "$dir\vitals-64.png"
$all = $pngs
$pngs = @($all | Where-Object { $_.Size -le 256 })

# .ico: ICONDIR, one 16-byte entry per image, then the PNG payloads.
$ico = New-Object IO.MemoryStream; $w = New-Object IO.BinaryWriter $ico
$w.Write([uint16]0); $w.Write([uint16]1); $w.Write([uint16]$pngs.Count)
$offset = 6 + 16 * $pngs.Count
foreach ($p in $pngs) {
    $w.Write([byte]($p.Size % 256)); $w.Write([byte]($p.Size % 256)); $w.Write([byte]0); $w.Write([byte]0)
    $w.Write([uint16]1); $w.Write([uint16]32); $w.Write([uint32]$p.Bytes.Length); $w.Write([uint32]$offset)
    $offset += $p.Bytes.Length
}
foreach ($p in $pngs) { $w.Write($p.Bytes) }
[IO.File]::WriteAllBytes("$dir\vitals.ico", $ico.ToArray())

# .res: an empty lead entry, one RT_ICON (3) per image, then RT_GROUP_ICON (14) named 1.
$res = New-Object IO.MemoryStream; $r = New-Object IO.BinaryWriter $res
function Entry([int]$type, [int]$id, [byte[]]$data, [int]$flags) {
    $r.Write([uint32]$data.Length); $r.Write([uint32]32)
    $r.Write([uint16]0xFFFF); $r.Write([uint16]$type); $r.Write([uint16]0xFFFF); $r.Write([uint16]$id)
    $r.Write([uint32]0); $r.Write([uint16]$flags); $r.Write([uint16]0x0409); $r.Write([uint32]0); $r.Write([uint32]0)
    $r.Write($data)
    while ($res.Length % 4) { $r.Write([byte]0) }
}
$r.Write([uint32]0); $r.Write([uint32]32); $r.Write([uint16]0xFFFF); $r.Write([uint16]0); $r.Write([uint16]0xFFFF); $r.Write([uint16]0); $r.Write([byte[]]::new(16))
$group = New-Object IO.MemoryStream; $g = New-Object IO.BinaryWriter $group
$g.Write([uint16]0); $g.Write([uint16]1); $g.Write([uint16]$pngs.Count)
$id = 1
foreach ($p in $pngs) {
    Entry 3 $id $p.Bytes 0x1010
    $g.Write([byte]($p.Size % 256)); $g.Write([byte]($p.Size % 256)); $g.Write([byte]0); $g.Write([byte]0)
    $g.Write([uint16]1); $g.Write([uint16]32); $g.Write([uint32]$p.Bytes.Length); $g.Write([uint16]$id)
    $id++
}
Entry 14 1 $group.ToArray() 0x1030
[IO.File]::WriteAllBytes("$dir\vitals.res", $res.ToArray())

# .icns: 'icns' + big-endian total length, then (type, big-endian length incl. 8-byte header, PNG).
$types = [ordered]@{ icp4 = 16; icp5 = 32; ic11 = 32; icp6 = 64; ic12 = 64; ic07 = 128; ic08 = 256; ic13 = 256; ic09 = 512; ic14 = 512; ic10 = 1024 }
function BE([uint32]$n) { $b = [BitConverter]::GetBytes($n); [Array]::Reverse($b); $b }
$body = New-Object IO.MemoryStream
foreach ($t in $types.Keys) {
    $png = ($all | Where-Object { $_.Size -eq $types[$t] }).Bytes
    $body.Write([Text.Encoding]::ASCII.GetBytes($t), 0, 4); $body.Write((BE ($png.Length + 8)), 0, 4); $body.Write($png, 0, $png.Length)
}
$icns = New-Object IO.MemoryStream
$icns.Write([Text.Encoding]::ASCII.GetBytes('icns'), 0, 4); $icns.Write((BE ($body.Length + 8)), 0, 4); $body.WriteTo($icns)
New-Item -ItemType Directory -Force "$dir\macos" | Out-Null
[IO.File]::WriteAllBytes("$dir\macos\vitals.icns", $icns.ToArray())
"{0} images, ico {1:N0} bytes, res {2:N0} bytes, icns {3:N0} bytes" -f $all.Count, (Get-Item "$dir\vitals.ico").Length, $res.Length, $icns.Length
