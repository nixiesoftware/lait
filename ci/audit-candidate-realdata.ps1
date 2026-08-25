# audit-candidate-realdata.ps1 — exercise an exact host candidate against the
# verified live-data snapshot, with the exact Issues World candidate installed.
#
# Everything here is disposable: the snapshot is extracted into a fresh audit
# home, the World candidate is installed only there, and the audit ends by
# proving every original snapshot file byte-identical. The live Space is never
# touched.
#
# The snapshot archive holds the ORBIT STORE — the contents of the live home's
# orbital/<space>/ directory. It is extracted to that same place under the
# audit home, because a Space store is discovered at <home>/orbital/<ws_...>
# and nowhere else; a registry row whose path holds no such store is
# navigation for a store that is "gone", and every use of it fails.
#
# Two acts cross the migration, and the audit performs both the way the
# fleet does. The representation rebuild (implicit prior journal -> explicit
# verified generation) is requested explicitly (host_orbit_rebuild) so its
# counts can be pinned to the verified replay. The World implementation
# adoption (the Space's active implementation advancing to the installed
# release's, with the 0.9.3 runner migrating its own records) is driven by
# the daemon's own consent lifecycle: the harness writes the durable
# upgrade.json consent record already naming the staged release, because the
# lifecycle's fetch phase resolves the real channel — which an isolated
# pre-publication audit deliberately cannot reach — and the staged release
# here IS the installed candidate.
#
# Usage:
#   ci/audit-candidate-realdata.ps1 `
#     -Candidate <lait.exe from the verified host candidate> `
#     -Archive <live-data snapshot .tar of the orbit store> `
#     -WorldBundle <world-bundles-x86_64-pc-windows-msvc.tar.gz from the
#                   verified world-candidate artifact> `
#     -IssuesVersion 0.9.3 `
#     -IdentityKey <identity secret.key> `
#     -AuditRoot <fresh directory>
#
# Verify the World candidate's checksums and attestations BEFORE handing its
# bundle to this script — the same verification ci/publish-world.sh --from-run
# performs. This script trusts the bytes it is given.
param(
    [Parameter(Mandatory = $true)] [string] $Candidate,
    [Parameter(Mandatory = $true)] [string] $Archive,
    [Parameter(Mandatory = $true)] [string] $WorldBundle,
    [Parameter(Mandatory = $true)] [string] $IssuesVersion,
    [Parameter(Mandatory = $true)] [string] $IdentityKey,
    [Parameter(Mandatory = $true)] [string] $AuditRoot,
    [string] $Space = "ws_38TLCQUD96NG9376CBELI5I5V2",
    [int] $ExpectedFiles = 9188,
    [int] $ExpectedEffects = 56,
    [int] $ExpectedBodies = 463,
    [int] $ExpectedReceipts = 5292
)

$ErrorActionPreference = "Stop"
if (Test-Path -LiteralPath $AuditRoot) {
    throw "audit root already exists: $AuditRoot"
}

$orbitDir = Join-Path $AuditRoot "orbital\$Space"
New-Item -ItemType Directory -Path $orbitDir | Out-Null
tar.exe -xf $Archive -C $orbitDir
Copy-Item -LiteralPath $IdentityKey -Destination (Join-Path $AuditRoot "secret.key")

# Install the exact Issues candidate the way the product records an
# installation: an immutable release directory, its release record, and the
# selection that names it. The recorded digest is the SHA-256 of the exact
# attested candidate archive this install came from — the artifact a channel
# publish would seal — so the install stays traceable to the audited bytes.
$worldId = "com.lait.issues"
$worldRoot = Join-Path $AuditRoot "world-bundles-v1\$worldId"
$extract = Join-Path $AuditRoot "world-candidate-extract"
New-Item -ItemType Directory -Path $extract | Out-Null
tar.exe -xzf $WorldBundle -C $extract
$release = Join-Path $extract "worlds\$worldId\$IssuesVersion"
if (-not (Test-Path -LiteralPath $release)) {
    throw "the World candidate bundle carries no $worldId $IssuesVersion"
}
New-Item -ItemType Directory -Path (Join-Path $worldRoot "releases") -Force | Out-Null
New-Item -ItemType Directory -Path (Join-Path $worldRoot "records") -Force | Out-Null
Move-Item -LiteralPath $release -Destination (Join-Path $worldRoot "releases\$IssuesVersion")
Remove-Item -LiteralPath $extract -Recurse -Force

$releaseFiles = @(Get-ChildItem `
    -LiteralPath (Join-Path $worldRoot "releases\$IssuesVersion") `
    -Recurse -File).Count
if (-not $releaseFiles) {
    throw "the installed $worldId $IssuesVersion release is empty"
}
$bundleDigest = (Get-FileHash -Algorithm SHA256 -LiteralPath $WorldBundle).Hash.ToLowerInvariant()
$record = [ordered]@{
    world = $worldId
    version = $IssuesVersion
    digest = $bundleDigest
    files = $releaseFiles
} | ConvertTo-Json
foreach ($recordPath in @(
    (Join-Path $worldRoot "records\$IssuesVersion.json"),
    (Join-Path $worldRoot "selected.json")
)) {
    [System.IO.File]::WriteAllText(
        $recordPath,
        $record,
        [System.Text.UTF8Encoding]::new($false)
    )
}

# The census covers the prior store exactly: everything the snapshot put under
# orbital/. The migration must only ADD (a generation directory, its selection);
# every durable ledger file must stay byte-identical.
#
# Node-local operational state is exempt: the store's `epoch` file is durably
# incremented on every Station activation (Beacon freshness — a live Station
# must never reuse an epoch it acted under), so starting the daemon at all
# advances it by one, entirely independent of the migration. Lock files are
# likewise runtime-only. These are not prior data; excluding them keeps the
# census a test of the migration's non-destructiveness, not of whether a
# daemon started.
$storeRoot = Join-Path $AuditRoot "orbital"
$nodeLocalNames = @("epoch", "epoch.tmp", "lock", "active-generation.lock")
$original = @(Get-ChildItem -LiteralPath $storeRoot -Recurse -File | Where-Object {
    $nodeLocalNames -notcontains $_.Name
})
if ($original.Count -ne $ExpectedFiles) {
    throw "expected $ExpectedFiles durable source files, got $($original.Count)"
}

$before = @{}
foreach ($file in $original) {
    $relative = $file.FullName.Substring($AuditRoot.Length + 1)
    $before[$relative] = [pscustomobject]@{
        Length = $file.Length
        Hash = (Get-FileHash -Algorithm SHA256 -LiteralPath $file.FullName).Hash
    }
}

$catalog = @([ordered]@{
    space = $Space
    name = "ISSUEWORLD"
    path = (Get-Item -LiteralPath $AuditRoot).FullName
    origin = "founded"
    host_nick = ""
    last_opened = 0
}) | ConvertTo-Json -AsArray
[System.IO.File]::WriteAllText(
    (Join-Path $AuditRoot "spaces.json"),
    $catalog,
    [System.Text.UTF8Encoding]::new($false)
)

$ready = Join-Path $AuditRoot "candidate-ready.json"
$log = Join-Path $AuditRoot "candidate-head.log"
$env:LAIT_NETWORK = "isolated"
$env:LAIT_IDLE_SECS = "0"
$env:LAIT_CONFIG_ROOT = $AuditRoot
$process = Start-Process `
    -FilePath $Candidate `
    -ArgumentList @("--json", "--port", "0", "--home", $AuditRoot, "--world", "issues") `
    -RedirectStandardOutput $ready `
    -RedirectStandardError $log `
    -WindowStyle Hidden `
    -PassThru

try {
    $deadline = [DateTime]::UtcNow.AddMinutes(4)
    $announcement = $null
    do {
        Start-Sleep -Milliseconds 250
        if (Test-Path -LiteralPath $ready) {
            $raw = Get-Content -LiteralPath $ready -Raw
            if ($raw -and $raw.Trim()) {
                try {
                    $candidateReady = $raw | ConvertFrom-Json
                    if ($candidateReady.port -and $candidateReady.token) {
                        $announcement = $candidateReady
                    }
                } catch {
                    # The readiness record may be between write and flush.
                }
            }
        }
        if ($process.HasExited -and -not $announcement) {
            throw "candidate exited $($process.ExitCode): $raw $(Get-Content -LiteralPath $log -Raw)"
        }
    } while (-not $announcement -and [DateTime]::UtcNow -lt $deadline)

    if (-not $announcement) {
        throw "candidate did not announce: $(Get-Content -LiteralPath $ready -Raw) $(Get-Content -LiteralPath $log -Raw)"
    }

    $headers = @{ Authorization = "Bearer $($announcement.token)" }
    $listing = Invoke-RestMethod `
        -Method Get `
        -Uri "http://127.0.0.1:$($announcement.port)/api/spaces" `
        -Headers $headers
    $spaces = @($listing.spaces)
    if ($spaces.Count -ne 1 -or -not $spaces[0].id) {
        throw "expected the self-contained legacy Space, got $($spaces | ConvertTo-Json -Compress)"
    }
    $orbit = $spaces[0].id

    # Cross the representation boundary with the exact candidate binary: the
    # implicit prior journal becomes an explicit, verified, activated
    # generation. This is the migration the fleet performs through the World
    # upgrade lifecycle, and its counts are pinned to the verified replay.
    $rebuild = Invoke-RestMethod `
        -Method Post `
        -Uri "http://127.0.0.1:$($announcement.port)/api/host/rpc" `
        -Headers $headers `
        -ContentType "application/json" `
        -Body (@{ cmd = "host_orbit_rebuild"; orbit = $orbit } | ConvertTo-Json -Compress)
    if ([int]$rebuild.effects -ne $ExpectedEffects `
        -or [int]$rebuild.bodies -ne $ExpectedBodies `
        -or [int]$rebuild.receipts -ne $ExpectedReceipts) {
        throw "rebuild diverged from the verified replay: $($rebuild | ConvertTo-Json -Compress)"
    }

    # Consent to the World update the way the fleet records it, with the
    # staged release already named: the daemon's lifecycle loop picks the
    # durable record up within a tick and advances the Space's active World
    # implementation under the installed candidate runner.
    $operation = [byte[]]::new(16)
    [System.Security.Cryptography.RandomNumberGenerator]::Fill($operation)
    $jobPath = Join-Path $worldRoot "upgrade.json"
    $job = [ordered]@{
        format = 2
        world = $worldId
        operation = @($operation)
        phase = "migrating"
        staged_version = $IssuesVersion
        current_orbit = $null
        after_orbit = $null
        completed_spaces = 0
        total_spaces = 0
        completed_records = 0
        remaining_records = $null
        message = $null
        updated_at = [uint64][DateTimeOffset]::UtcNow.ToUnixTimeSeconds()
    } | ConvertTo-Json -Compress
    $jobStaging = "$jobPath.staging"
    [System.IO.File]::WriteAllText($jobStaging, $job, [System.Text.UTF8Encoding]::new($false))
    Move-Item -LiteralPath $jobStaging -Destination $jobPath -Force

    # The migration is size-proportional, so the wait is progress-aware: as
    # long as completed_records advances the clock resets; only a genuine
    # stall or the absolute ceiling fails the audit.
    $absoluteDeadline = [DateTime]::UtcNow.AddMinutes(90)
    $stallDeadline = [DateTime]::UtcNow.AddMinutes(5)
    $lastProgress = -1
    $finalPhase = $null
    do {
        Start-Sleep -Milliseconds 500
        try {
            $state = Get-Content -LiteralPath $jobPath -Raw | ConvertFrom-Json
            if ($state.phase -in @("verified", "refused")) { $finalPhase = $state }
            elseif ([int]$state.completed_records -gt $lastProgress) {
                $lastProgress = [int]$state.completed_records
                $stallDeadline = [DateTime]::UtcNow.AddMinutes(5)
            }
        } catch {
            # The daemon replaces the record atomically; a read may land between.
        }
    } while (-not $finalPhase `
        -and [DateTime]::UtcNow -lt $stallDeadline `
        -and [DateTime]::UtcNow -lt $absoluteDeadline)
    if (-not $finalPhase) {
        throw "the World update lifecycle did not conclude: $(Get-Content -LiteralPath $jobPath -Raw)"
    }
    if ($finalPhase.phase -ne "verified") {
        throw "the World update lifecycle refused: $($finalPhase | ConvertTo-Json -Compress)"
    }

    $issueList = Invoke-RestMethod `
        -Method Post `
        -Uri "http://127.0.0.1:$($announcement.port)/api/spaces/$orbit/worlds/issues/rpc" `
        -Headers $headers `
        -ContentType "application/json" `
        -Body '{"cmd":"list","page":{}}'
    if (-not $issueList -or -not @($issueList.issues).Count) {
        throw "the migrated Issues view was empty: $($issueList | ConvertTo-Json -Compress)"
    }

    $response = Invoke-RestMethod `
        -Method Post `
        -Uri "http://127.0.0.1:$($announcement.port)/api/host/rpc" `
        -Headers $headers `
        -ContentType "application/json" `
        -Body '{"cmd":"host_restart"}'
    if ($response.host -ne "restarting") {
        throw "host_restart returned $($response | ConvertTo-Json -Compress)"
    }
    Start-Sleep -Seconds 2
    $process.Kill($true)
    $process.WaitForExit()
} finally {
    if (-not $process.HasExited) {
        $process.Kill($true)
        $process.WaitForExit()
    }
}

$changed = @()
$missing = @()
foreach ($relative in $before.Keys) {
    $path = Join-Path $AuditRoot $relative
    if (-not (Test-Path -LiteralPath $path)) {
        $missing += $relative
        continue
    }
    $now = Get-Item -LiteralPath $path
    $hash = (Get-FileHash -Algorithm SHA256 -LiteralPath $path).Hash
    if ($now.Length -ne $before[$relative].Length -or $hash -ne $before[$relative].Hash) {
        $changed += $relative
    }
}

if ($missing.Count -or $changed.Count) {
    throw "source mutation: missing=$($missing.Count), changed=$($changed.Count)"
}

$activeGeneration = Join-Path $orbitDir "active-generation"
if (-not (Test-Path -LiteralPath $activeGeneration)) {
    throw "active-generation absent after migration"
}

[pscustomobject]@{
    ReadyPort = $announcement.port
    Orbit = $orbit
    IssuesRelease = "$worldId $IssuesVersion ($releaseFiles files, sha256 $bundleDigest)"
    Rebuild = ($rebuild | ConvertTo-Json -Compress)
    Adoption = ($finalPhase | ConvertTo-Json -Compress)
    MigratedIssueRows = @($issueList.issues).Count
    HostRestart = ($response | ConvertTo-Json -Compress)
    OriginalFiles = $before.Count
    Missing = $missing.Count
    Changed = $changed.Count
    ActiveGeneration = (Get-Content -LiteralPath $activeGeneration -Raw).Trim()
    GenerationDirectories = @(Get-ChildItem -LiteralPath (Join-Path $orbitDir "generations") -Directory).Count
    TotalFiles = @(Get-ChildItem -LiteralPath $AuditRoot -Recurse -File).Count
} | Format-List
