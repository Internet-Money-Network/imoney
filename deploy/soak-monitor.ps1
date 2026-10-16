# Soak-run monitor: every interval, asks each node for its block count, latest block, peers,
# blocks held in memory, memory and disk, and appends one line per node to a CSV file. A run is
# healthy when the nodes keep the same latest block, memory levels off, and disk grows at the
# expected rate.
#
#   powershell -File deploy\soak-monitor.ps1 -Project <gcloud project> [-IntervalMinutes 30]
#
# Every node is read by running one command on its machine: the home node over SSH, the Google
# Cloud nodes over gcloud's tunnel. Each read has a time limit, so one machine that does not
# answer cannot stall the monitor.
param(
    [Parameter(Mandatory = $true)][string]$Project,
    [int]$IntervalMinutes = 30,
    [string]$Out = "data\soak.csv",
    [string]$HomeHost = "timpisynaptron@192.168.1.69",
    [int]$TimeoutSeconds = 120
)

$gcloud = "$env:LOCALAPPDATA\Google\Cloud SDK\google-cloud-sdk\bin\gcloud.cmd"
$nodes = @(
    @{ Name = "home"; Kind = "ssh" },
    @{ Name = "us"; Kind = "gcloud"; Instance = "imn-seed-us"; Zone = "us-central1-a" },
    @{ Name = "eu"; Kind = "gcloud"; Instance = "imn-seed-eu"; Zone = "europe-west1-b" },
    @{ Name = "asia"; Kind = "gcloud"; Instance = "imn-seed-asia"; Zone = "asia-southeast1-b" }
)
# No quotes in the remote command: it passes through PowerShell, gcloud and plink
$remote = "curl -s -m 10 http://127.0.0.1:18556/api/v1/stats; echo; curl -s -m 10 http://127.0.0.1:18556/api/v1/info; echo; sudo -n docker stats --no-stream imoney-node 2>/dev/null || docker stats --no-stream imoney-node; df -BG /"

if (-not (Test-Path $Out)) { "time,node,blocks,tip,peers,mempool,memory,disk_gb,in_memory" | Set-Content -Encoding ascii $Out }

function Read-Node($node) {
    $job = if ($node.Kind -eq "ssh") {
        Start-Job -ScriptBlock { param($h, $c) ssh -o ConnectTimeout=20 -o BatchMode=yes $h $c 2>$null } -ArgumentList $HomeHost, $remote
    } else {
        Start-Job -ScriptBlock { param($g, $n, $p, $c) "y" | & $g compute ssh $n.Instance --zone $n.Zone --project $p --quiet --tunnel-through-iap --strict-host-key-checking=no --command $c 2>$null } -ArgumentList $gcloud, $node, $Project, $remote
    }
    $lines = if (Wait-Job $job -Timeout $TimeoutSeconds) { Receive-Job $job } else { @() }
    Remove-Job $job -Force
    return $lines
}

while ($true) {
    $now = (Get-Date).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ssZ")
    foreach ($n in $nodes) {
        $lines = @(Read-Node $n)
        $json = @($lines | Where-Object { $_ -like "{*" })
        try {
            $stats = $json | Where-Object { $_ -like "*hashrate*" } | Select-Object -First 1 | ConvertFrom-Json
            $info = $json | Where-Object { $_ -like "*genesis_hash*" } | Select-Object -First 1 | ConvertFrom-Json
            # "imoney-node ... 73.4MiB / 1.465GiB ..." and "/dev/sda1 30G 9G 20G 30% /"
            $memory = ($lines | Select-String "(\d[\d.]*\S*iB)\s*/\s*\S+iB" | Select-Object -First 1).Matches.Groups[1].Value
            $disk = ($lines | Select-String "^/dev/\S+\s+\S+\s+(\d+)G" | Select-Object -First 1).Matches.Groups[1].Value
            "$now,$($n.Name),$($info.total_blocks),$($info.virtual_selected_parent.Substring(0,16)),$($stats.peers),$($info.mempool_size),$memory,$disk,$($info.blocks_in_memory)" | Add-Content -Encoding ascii $Out
        } catch {
            "$now,$($n.Name),unreachable,,,,,," | Add-Content -Encoding ascii $Out
        }
    }
    Start-Sleep -Seconds ($IntervalMinutes * 60)
}
