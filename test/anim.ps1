# 30 fps animated test window ("FlowTest") for FlowScale live tests
Add-Type -AssemblyName System.Windows.Forms, System.Drawing
$f = New-Object Windows.Forms.Form; $f.Text = 'FlowTest'; $f.ClientSize = '640,360'; $f.StartPosition = 'CenterScreen'
$p = $f.GetType().GetProperty('DoubleBuffered', [Reflection.BindingFlags]'NonPublic,Instance'); $p.SetValue($f, $true)
$script:t = 0
$f.Add_Paint({ param($s, $e) $g = $e.Graphics; $g.Clear([Drawing.Color]::FromArgb(17,34,51))
  for ($i = 0; $i -lt 8; $i++) { $b = New-Object Drawing.SolidBrush ([Drawing.Color]::FromArgb(255, (40+$i*27)%256, (200-$i*20)%256, 120)); $g.FillRectangle($b, (($script:t*4 + $i*90) % 700) - 60, 40 + $i*38, 60, 30) }
  $g.DrawString("HUD $script:t", (New-Object Drawing.Font 'Consolas', 14), [Drawing.Brushes]::White, 10, 6) })
$tm = New-Object Windows.Forms.Timer; $tm.Interval = 33; $tm.Add_Tick({ $script:t++; $f.Invalidate() }); $tm.Start()
$c = New-Object Windows.Forms.Timer; $c.Interval = [int]($args[0]) * 1000; if ($c.Interval -le 0) { $c.Interval = 20000 }; $c.Add_Tick({ $f.Close() }); $c.Start()
[Windows.Forms.Application]::Run($f)
