# audit-candidate-realdata.ps1 — exercise an exact host candidate against the
# verified live-data snapshot, with the exact Issues World candidate installed.
#
# Everything here is disposable: the snapshot is extracted into a fresh audit
# root, the World candidate is installed only there, and the audit ends by
# proving every original snapshot file byte-identical. The live Space is never
# touched.
#
# Usage:
#   ci/audit-candidate-realdata.ps1 `
#     -Candidate <lait.exe from the verified host candidate> `
#     -Archive <live-data snapshot .tar> `
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
    [Parameter(Mandatory = $true)] [string] $AuditRoot
)

$ErrorActionPreference = "Stop"
if (Test-Path -LiteralPath $AuditRoot) {
    throw "audit root already exists: $AuditRoot"
}

New-Item -ItemType Directory -Path $AuditRoot | Out-Null
tar.exe -xf $Archive -C $AuditRoot
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

$excludedWorlds = "$AuditRoot\world-bundles-v1\*"
$original = @(Get-ChildItem -LiteralPath $AuditRoot -Recurse -File | Where-Object {
    $_.FullName -notlike $excludedWorlds -and $_.Name -ne "secret.key"
})
if ($original.Count -ne 9189) {
    throw "expected 9189 source files, got $($original.Count)"
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
    space = "ws_38TLCQUD96NG9376CBELI5I5V2"
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
    $issueList = Invoke-RestMethod `
        -Method Post `
        -Uri "http://127.0.0.1:$($announcement.port)/api/spaces/$orbit/worlds/issues/rpc" `
        -Headers $headers `
        -ContentType "application/json" `
        -Body '{"cmd":"list","page":{}}'
    if (-not $issueList) {
        throw "the migrated Issues view was empty"
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

$activeGeneration = Join-Path $AuditRoot "active-generation"
if (-not (Test-Path -LiteralPath $activeGeneration)) {
    throw "active-generation absent after migration"
}

[pscustomobject]@{
    ReadyPort = $announcement.port
    Orbit = $orbit
    IssuesRelease = "$worldId $IssuesVersion ($releaseFiles files, sha256 $bundleDigest)"
    MigratedIssueRows = @($issueList.issues).Count
    HostRestart = ($response | ConvertTo-Json -Compress)
    OriginalFiles = $before.Count
    Missing = $missing.Count
    Changed = $changed.Count
    ActiveGeneration = (Get-Content -LiteralPath $activeGeneration -Raw).Trim()
    GenerationDirectories = @(Get-ChildItem -LiteralPath (Join-Path $AuditRoot "generations") -Directory).Count
    TotalFiles = @(Get-ChildItem -LiteralPath $AuditRoot -Recurse -File).Count
} | Format-List
