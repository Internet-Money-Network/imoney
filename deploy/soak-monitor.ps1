# Soak-run monitor: every interval, asks each node for its block count, latest block, peers,
# memory and disk, and appends one line per node to a CSV file. A run is healthy when the
# nodes keep the same latest block, memory levels off, and disk grows at the expected rate.
#
#   powershell -File deploy\soak-monitor.ps1 -Project <gcloud project> [-IntervalMinutes 30]
#
# The home node is read through an SSH tunnel on 127.0.0.1:18556; the Google Cloud nodes are
# read over gcloud's HTTPS tunnel, since their API is not exposed.
param(
    [Parameter(Mandatory = $true)][string]$Project,
    [int]$IntervalMinutes = 30,
    [string]$Out = "data\soak.csv",
    [string]$HomeApi = "http://127.0.0.1:18556"
)

$gcloud = "$env:LOCALAPPDATA\Google\Cloud SDK\google-cloud-sdk\bin\gcloud.cmd"
$cloud = @(
    @{ Name = "us"; Instance = "imn-seed-us"; Zone = "us-central1-a" },
    @{ Name = "eu"; Instance = "imn-seed-eu"; Zone = "europe-west1-b" },
    @{ Name = "asia"; Instance = "imn-seed-asia"; Zone = "asia-southeast1-b" }
)
# No quotes in the remote command: it passes through PowerShell, gcloud and plink
$remote = "curl -s -m 10 http://127.0.0.1:18556/api/v1/stats; echo; sudo docker stats --no-stream imoney-node; df -BG /"

if (-not (Test-Path $Out)) { "time,node,blocks,tip,peers,mempool,memory,disk_gb" | Set-Content -Encoding ascii $Out }

while ($true) {
    $now = (Get-Date).ToUniversalTime().ToString("yyyy-MM-ddTHH:mm:ssZ")
    try {
        $s = Invoke-RestMethod -Uri "$HomeApi/api/v1/stats" -TimeoutSec 15
        "$now,home,$($s.total_blocks),$($s.selected_tip.Substring(0,16)),$($s.peers),$($s.mempool_size),," | Add-Content -Encoding ascii $Out
    } catch {
        "$now,home,unreachable,,,,," | Add-Content -Encoding ascii $Out
    }
    foreach ($n in $cloud) {
        $lines = "y" | & $gcloud compute ssh $n.Instance --zone $n.Zone --project $Project --quiet --tunnel-through-iap --strict-host-key-checking=no --command $remote 2>$null
        $json = $lines | Where-Object { $_ -like "{*" } | Select-Object -Last 1
        if ($json) {
            $s = $json | ConvertFrom-Json
            # "imoney-node ... 73.4MiB / 1.465GiB ..." and "/dev/sda1 30G 9G 20G 30% /"
            $memory = ($lines | Select-String "(\S+)\s*/\s*\S+iB" | Select-Object -First 1).Matches.Groups[1].Value
            $disk = ($lines | Select-String "^/dev/\S+\s+\S+\s+(\d+)G" | Select-Object -First 1).Matches.Groups[1].Value
            "$now,$($n.Name),$($s.total_blocks),$($s.selected_tip.Substring(0,16)),$($s.peers),$($s.mempool_size),$memory,$disk" | Add-Content -Encoding ascii $Out
        } else {
            "$now,$($n.Name),unreachable,,,,," | Add-Content -Encoding ascii $Out
        }
    }
    Start-Sleep -Seconds ($IntervalMinutes * 60)
}
