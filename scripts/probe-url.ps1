param(
    [Parameter(Mandatory)][string]$Url,
    [int]$Connections = 16,
    [string]$Referer
)
# How a download server behaves, from a few requests that read only headers (never the whole file): size, Range
# support, the most connections it accepts at once, bot checks. When an address fails in EDM, this tells which
# scenario in src-tauri/src/segmented/scenarios.rs it needs.
$ErrorActionPreference = 'Stop'
Add-Type -AssemblyName System.Net.Http
[Net.ServicePointManager]::SecurityProtocol = [Net.SecurityProtocolType]::Tls12
[Net.ServicePointManager]::DefaultConnectionLimit = 256
$handler = [System.Net.Http.HttpClientHandler]::new()
$handler.AutomaticDecompression = [System.Net.DecompressionMethods]::None
$client = [System.Net.Http.HttpClient]::new($handler)
$client.Timeout = [TimeSpan]::FromSeconds(30)
$ua = 'Mozilla/5.0 (Windows NT 10.0; Win64; x64; rv:144.0) Gecko/20100101 Firefox/144.0'

function Send([string]$range) {
    $r = [System.Net.Http.HttpRequestMessage]::new([System.Net.Http.HttpMethod]::Get, $Url)
    [void]$r.Headers.TryAddWithoutValidation('User-Agent', $ua)
    if ($Referer) { [void]$r.Headers.TryAddWithoutValidation('Referer', $Referer) }
    if ($range) { [void]$r.Headers.TryAddWithoutValidation('Range', $range) }
    $client.SendAsync($r, [System.Net.Http.HttpCompletionOption]::ResponseHeadersRead)
}
function Header($resp, [string]$name) {
    $v = $null
    if ($resp.Headers.TryGetValues($name, [ref]$v)) { return ($v -join ', ') }
    if ($resp.Content -and $resp.Content.Headers.TryGetValues($name, [ref]$v)) { return ($v -join ', ') }
    ''
}

$probe = (Send 'bytes=0-0').Result
$status = [int]$probe.StatusCode
$total = -1
$cr = Header $probe 'Content-Range'
if ($cr -match '/(\d+)$') { $total = [long]$Matches[1] } elseif ($status -eq 200) { $total = [long]("0" + (Header $probe 'Content-Length')) }
'Probe (Range 0-0)  : HTTP {0}' -f $status
'Final address      : {0}' -f $probe.RequestMessage.RequestUri
'Content-Type       : {0}' -f (Header $probe 'Content-Type')
'Content-Disposition: {0}' -f (Header $probe 'Content-Disposition')
'Size               : {0}' -f $(if ($total -gt 0) { '{0:N0} bytes' -f $total } else { 'unknown' })
'Range support      : {0}' -f $(if ($status -eq 206) { 'yes (206)' } elseif ((Header $probe 'Accept-Ranges') -match 'bytes') { 'claimed, but answered 200' } else { 'no' })
'Server             : {0}' -f (Header $probe 'Server')
if ((Header $probe 'cf-mitigated') -or (($status -eq 403 -or $status -eq 503) -and (Header $probe 'Server') -match 'cloudflare|ddos-guard')) {
    'Bot check          : yes (only the browser gets the file)'
}
$probe.Dispose()
if ($status -ne 206) { return }

# Connections held open at once, each asking for a different part, as EDM's segments do.
$step = [math]::Max([long]1, [long]($total / $Connections))
$tasks = @(0..($Connections - 1) | ForEach-Object { Send ('bytes={0}-' -f ($_ * $step)) })
try { [System.Threading.Tasks.Task]::WaitAll($tasks) } catch {}
$codes = $tasks | ForEach-Object { if ($_.IsFaulted) { 'error' } else { [int]$_.Result.StatusCode } }
$ok = @($codes | Where-Object { $_ -eq 206 }).Count
'Connections at once: {0} of {1} accepted' -f $ok, $Connections
$codes | Group-Object | Where-Object { $_.Name -ne '206' } | ForEach-Object { '  refused           : {0} x HTTP {1}' -f $_.Count, $_.Name }
$tasks | Where-Object { -not $_.IsFaulted } | ForEach-Object { $_.Result.Dispose() }
