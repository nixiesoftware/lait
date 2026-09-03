sub init()
    m.top.functionName = "AstrolabeRun"
end sub

sub AstrolabePublish(model as object)
    if not model.DoesExist("transport") then model.transport = m.transport
    if not model.DoesExist("source") then model.source = "none"
    if not model.DoesExist("stale") then model.stale = false
    m.top.viewModel = model
end sub

function AstrolabeHeader(event as object, wanted as string) as dynamic
    found = invalid
    for each entry in event.GetResponseHeadersArray()
        for each name in entry
            if LCase(name) = LCase(wanted)
                if found <> invalid then return invalid
                found = entry[name]
            end if
        end for
    end for
    return found
end function

function AstrolabeTransfer(path as string, method as string, body as string, headers as object, timeoutMs as integer, targetFile = invalid as dynamic, interruptible = false as boolean) as dynamic
    transfer = CreateObject("roUrlTransfer")
    transfer.SetPort(m.port)
    transfer.SetCertificatesFile(m.certificates)
    transfer.EnablePeerVerification(true)
    ' A pinned certificate is the identity; the names inside it are where it
    ' once lived. Web PKI trust still checks the host.
    transfer.EnableHostVerification(m.trustKind <> "pinned_certificate")
    transfer.RetainBodyOnError(true)
    transfer.SetUrl(m.origin + path)
    transfer.AddHeader("Accept", "application/json")
    for each name in headers
        transfer.AddHeader(name, headers[name])
    end for
    issued = false
    if method = "POST"
        transfer.AddHeader("Content-Type", "application/json; charset=utf-8")
        issued = transfer.AsyncPostFromString(body)
    else if targetFile <> invalid
        issued = transfer.AsyncGetToFile(targetFile)
    else
        issued = transfer.AsyncGetToString()
    end if
    if not issued then return invalid

    clock = CreateObject("roTimespan")
    clock.Mark()
    while clock.TotalMilliseconds() < timeoutMs
        event = Wait(100, m.port)
        AstrolabeTickPlayback()
        if m.credential <> invalid
            if m.credential.mode = "pairing" and Left(m.top.command, 7) = "cancel:"
                transfer.AsyncCancel()
                AstrolabeClearCredential()
                m.credential = invalid
                return invalid
            end if
            ' A failed player interrupts only the long poll it would otherwise
            ' wait behind. Cancelling every transfer on it cancelled the very
            ' re-staging that answers the failure.
            if interruptible and m.credential.mode = "paired" and Left(m.top.command, 13) = "media_failed:"
                transfer.AsyncCancel()
                return invalid
            end if
        end if
        if event <> invalid and Type(event) = "roUrlEvent" and event.GetSourceIdentity() = transfer.GetIdentity()
            result = { status: event.GetResponseCode(), event: event, body: "" }
            if targetFile = invalid then result.body = event.GetString()
            ' A refused or failed transfer is said on the console, with the
            ' coordinator's reason, so a receiver that is being turned away is
            ' a line to read rather than a guess.
            if result.status < 200 or result.status >= 300 then print "[astrolabe] transfer "; method; " "; path; " -> "; result.status; " "; event.GetFailureReason()
            return result
        end if
    end while
    transfer.AsyncCancel()
    return invalid
end function

function AstrolabeJson(path as string, method as string, value as dynamic, headers = invalid as dynamic, timeoutMs = 30000 as integer) as dynamic
    body = ""
    if value <> invalid then body = FormatJson(value)
    if headers = invalid then headers = {}
    result = AstrolabeTransfer(path, method, body, headers, timeoutMs)
    if result = invalid then return invalid
    bytes = AstrolabeByteArray(result.body)
    if bytes.Count() > 65536 then return invalid
    result.json = ParseJson(result.body)
    return result
end function

' What outlives this channel's own store: the device's channel client id,
' bound to the coordinator it is presented to so it names this receiver
' here and nowhere else.
function AstrolabeReceiverId(profile as string) as string
    bytes = AstrolabeTranscript("astrolabe-display/receiver-id/v1")
    AstrolabeU32Field(bytes, 1)
    AstrolabeTextField(bytes, profile)
    AstrolabeTextField(bytes, CreateObject("roDeviceInfo").GetChannelClientId())
    return AstrolabeSha256(bytes)
end function

function AstrolabeCapabilities() as object
    display = CreateObject("roDeviceInfo").GetDisplaySize()
    width = 1920
    height = 1080
    if display <> invalid
        width = display.w
        height = display.h
    end if
    return {
        protocol_major: 1,
        platform: "roku",
        build: AstrolabeBuild(),
        viewport: { width: width, height: height, scale_milli: 1000 },
        image_types: ["image_jpeg", "image_png", "image_webp"],
        max_asset_bytes: 16777216,
        max_staged_bytes: 50331648,
        max_program_items: 16,
        max_staging_horizon_ms: 86400000,
        locale: "en-US",
        accessibility: {
            native_screen_reader: true,
            spoken_summary: true,
            captions: false,
            audio_description: false
        },
        playback: {
            tier: "native_hls",
            sync_class: "none",
            rate_control_probed: false,
            latency_class: "broadcast",
            health_granularity: "coarse"
        }
    }
end function

function AstrolabeCurrentPlayback() as object
    if m.program = invalid then return { currentIndex: 0, elapsedMs: 0 }
    elapsed = m.elapsedBase + m.playbackClock.TotalMilliseconds()
    if elapsed > 2147483647 then elapsed = 2147483647
    return { currentIndex: m.program.playback.current_index, elapsedMs: elapsed }
end function

function AstrolabeContext(route as string, method as string, bodySha as string, overrides = invalid as dynamic) as object
    context = {
        method: method,
        route: route,
        device: m.credential.device,
        assignment: invalid,
        program: invalid,
        revision: invalid,
        currentItem: invalid,
        elapsedMs: invalid,
        waitMs: invalid,
        asset: invalid,
        range: invalid,
        challenge: m.challenge,
        bodySha256: bodySha
    }
    if m.program <> invalid
        playback = AstrolabeCurrentPlayback()
        context.assignment = m.program.assignment
        context.program = m.program.program
        context.revision = m.program.revision
        context.currentItem = m.program.items[playback.currentIndex].id
        context.elapsedMs = playback.elapsedMs
    end if
    if overrides <> invalid
        for each name in overrides
            context[name] = overrides[name]
        end for
    end if
    return context
end function

function AstrolabeHeaders(context as object, tag as string) as object
    headers = {
        "Authorization": "Astrolabe-HMAC " + tag,
        "X-Astrolabe-Protocol-Major": "1",
        "X-Astrolabe-Route": context.route,
        "X-Astrolabe-Device": context.device,
        "X-Astrolabe-Challenge": context.challenge,
        "X-Astrolabe-Body-SHA256": context.bodySha256
    }
    optional = {
        "X-Astrolabe-Assignment": context.assignment,
        "X-Astrolabe-Program": context.program,
        "X-Astrolabe-Revision": context.revision,
        "X-Astrolabe-Current-Item": context.currentItem,
        "X-Astrolabe-Elapsed-Ms": context.elapsedMs,
        "X-Astrolabe-Wait-Ms": context.waitMs,
        "X-Astrolabe-Asset": context.asset
    }
    for each name in optional
        if optional[name] <> invalid then headers[name] = optional[name].ToStr()
    end for
    if context.range <> invalid
        headers["X-Astrolabe-Range-Start"] = context.range.start.ToStr()
        headers["X-Astrolabe-Range-Length"] = context.range.length.ToStr()
        lastByte = context.range.start + context.range.length - 1
        headers["Range"] = "bytes=" + context.range.start.ToStr() + "-" + lastByte.ToStr()
    end if
    return headers
end function

function AstrolabeEnsureChallenge() as boolean
    if m.challenge <> invalid then return true
    response = AstrolabeJson("/head/v1/challenges", "POST", {
        protocol_major: 1,
        device: m.credential.device
    })
    if response = invalid or response.status <> 200 or response.json = invalid then return false
    if not AstrolabeExactFields(response.json, ["protocol_major", "challenge", "expires_in_ms"]) then return false
    if response.json.protocol_major <> 1 or not AstrolabeIsHex(response.json.challenge, 64) then return false
    if not AstrolabeIntegerIn(response.json.expires_in_ms, 1, 120000) then return false
    m.challenge = response.json.challenge
    return true
end function

function AstrolabeAuthorizedJson(route as string, method as string, path as string, value as dynamic, overrides = invalid as dynamic, timeoutMs = 30000 as integer) as dynamic
    if not AstrolabeEnsureChallenge() then return invalid
    body = ""
    if value <> invalid then body = FormatJson(value)
    bodySha = AstrolabeSha256(AstrolabeByteArray(body))
    context = AstrolabeContext(route, method, bodySha, overrides)
    headers = AstrolabeHeaders(context, AstrolabeRequestTag(m.credential.proofKey, context))
    m.challenge = invalid
    result = AstrolabeTransfer(path, method, body, headers, timeoutMs, invalid, route = "program_changes")
    if result = invalid
        m.transport = "offline"
        return invalid
    end if
    nextChallenge = AstrolabeHeader(result.event, "X-Astrolabe-Next-Challenge")
    if AstrolabeByteArray(result.body).Count() > 65536 then return invalid
    if not AstrolabeIsHex(nextChallenge, 64)
        ' A refusal carries no next challenge; revocation and re-pair arrive
        ' exactly this way, so the body is read before the header is required.
        if result.status < 400 then return invalid
        result.json = ParseJson(result.body)
        if not AstrolabeValidApiError(result.json) then return invalid
        return result
    end if
    m.challenge = nextChallenge
    m.transport = "online"
    result.json = ParseJson(result.body)
    return result
end function

function AstrolabeAuthorizedAsset(asset as object, program as object) as dynamic
    complete = CreateObject("roByteArray")
    start = 0
    chunkSize = 262144
    while start < asset.encoded_len
        length = chunkSize
        if start + length > asset.encoded_len then length = asset.encoded_len - start
        if not AstrolabeEnsureChallenge() then return invalid
        emptySha = AstrolabeSha256(CreateObject("roByteArray"))
        context = AstrolabeContext("asset", "GET", emptySha, {
            assignment: program.assignment,
            program: program.program,
            revision: program.revision,
            currentItem: invalid,
            elapsedMs: invalid,
            asset: asset.id,
            range: { start: start, length: length }
        })
        headers = AstrolabeHeaders(context, AstrolabeRequestTag(m.credential.proofKey, context))
        headers["Accept"] = AstrolabeMediaType(asset.media_type)
        chunkPath = "tmp:/astrolabe_chunk"
        CreateObject("roFileSystem").Delete(chunkPath)
        m.challenge = invalid
        result = AstrolabeTransfer("/head/v1/assets/" + asset.id, "GET", "", headers, 30000, chunkPath)
        if result = invalid or result.status <> 206 then return invalid
        nextChallenge = AstrolabeHeader(result.event, "X-Astrolabe-Next-Challenge")
        if not AstrolabeIsHex(nextChallenge, 64) then return invalid
        m.challenge = nextChallenge
        contentType = AstrolabeHeader(result.event, "Content-Type")
        if contentType = invalid or LCase(contentType) <> AstrolabeMediaType(asset.media_type) then return invalid
        expectedRange = "bytes " + start.ToStr() + "-" + (start + length - 1).ToStr() + "/" + asset.encoded_len.ToStr()
        if AstrolabeHeader(result.event, "Content-Range") <> expectedRange then return invalid
        stat = CreateObject("roFileSystem").Stat(chunkPath)
        if stat = invalid or stat.size <> length then return invalid
        chunk = CreateObject("roByteArray")
        if not chunk.ReadFile(chunkPath) or chunk.Count() <> length then return invalid
        AstrolabeAppend(complete, chunk)
        if complete.Count() > asset.encoded_len or complete.Count() > 16777216 then return invalid
        start = start + length
        m.transport = "online"
    end while
    if complete.Count() <> asset.encoded_len or AstrolabeSha256(complete) <> asset.sha256 then return invalid
    extension = ".bin"
    if asset.media_type = "image_png" then extension = ".png"
    if asset.media_type = "image_jpeg" then extension = ".jpg"
    if asset.media_type = "image_webp" then extension = ".webp"
    finalPath = "tmp:/astrolabe_" + Left(asset.id, 24) + extension
    if not complete.WriteFile(finalPath) then return invalid
    return { uri: finalPath, width: asset.width, height: asset.height, bytes: asset.encoded_len }
end function

function AstrolabeAuthorizedLive(item as object, program as object) as dynamic
    manifest = item.scene.manifest
    response = AstrolabeAuthorizedJson("live_ticket", "POST", "/head/v1/live/tickets", { transport: "hls" }, {
        assignment: program.assignment,
        program: program.program,
        revision: program.revision,
        currentItem: item.id,
        elapsedMs: 0,
        asset: manifest.id
    })
    if response = invalid or response.status <> 200 or response.json = invalid then return invalid
    ticket = response.json
    if not AstrolabeExactFields(ticket, ["protocol_major", "transport", "endpoint", "expires_at_unix_ms"]) then return invalid
    if ticket.protocol_major <> 1 or ticket.transport <> "hls" then return invalid
    if not AstrolabeIntegerIn(ticket.expires_at_unix_ms, 1, 9007199254740991) then return invalid
    matcher = CreateObject("roRegex", "^/head/v1/live/[0-9a-f]{64}/master[.]m3u8$", "")
    if not AstrolabeIsString(ticket.endpoint) or not matcher.IsMatch(ticket.endpoint) then return invalid
    ' The player runs its own TLS; it is handed the same pin the API calls use.
    return { uri: m.origin + ticket.endpoint, bytes: manifest.encoded_len, live: true, certificates: m.certificates }
end function

function AstrolabeRefreshLive() as boolean
    if m.program = invalid then return false
    playback = AstrolabeCurrentPlayback()
    item = m.program.items[playback.currentIndex]
    if item.scene.kind <> "media" then return false
    ' The failure is answered once. Restoring the command on a refused ticket
    ' made every later transfer cancel itself on the next tick — a livelock.
    m.top.command = ""
    entry = AstrolabeAuthorizedLive(item, m.program)
    if entry = invalid
        ' A ticket the coordinator will not renew is a program to fetch again:
        ' the snapshot re-stages every item with tickets the coordinator holds.
        AstrolabeRetireStage(m.stage)
        m.program = invalid
        m.stage = {}
        return false
    end if
    m.stage[item.scene.manifest.id] = entry
    AstrolabeRenderCurrent()
    return true
end function

function AstrolabeMediaType(kind as string) as string
    if kind = "image_png" then return "image/png"
    if kind = "image_jpeg" then return "image/jpeg"
    if kind = "image_webp" then return "image/webp"
    return "application/octet-stream"
end function

sub AstrolabePair()
    instance = AstrolabeJson("/head/v1/instance", "GET", invalid)
    if instance = invalid or instance.status <> 200 or instance.json = invalid
        AstrolabePublish({ kind: "message", title: "Coordinator unavailable", body: "" })
        return
    end if
    offer = instance.json
    if not AstrolabeExactFields(offer, ["protocol_major", "instance", "label", "profile", "trust"]) or offer.protocol_major <> 1
        AstrolabePublish({ kind: "message", title: "Coordinator refused", body: "Not Astrolabe Display 1." })
        return
    end if
    if not AstrolabeIsHex(offer.instance, 32) or not AstrolabeIsString(offer.label) or not AstrolabeIsProfileId(offer.profile) then return
    if Len(offer.label) < 1 or Len(offer.label) > 96 then return
    if m.trustKind = "web_pki_origin"
        trustMatches = AstrolabeExactFields(offer.trust, ["kind", "origin"]) and offer.trust.kind = m.trustKind and offer.trust.origin = m.origin
    else
        ' The pin is the certificate. The origin it advertises is where it
        ' believes it lives, which a moved coordinator gets wrong.
        trustMatches = AstrolabeExactFields(offer.trust, ["kind", "origin", "sha256"]) and offer.trust.kind = m.trustKind and offer.trust.sha256 = m.fingerprint
    end if
    m.coordinatorProfile = offer.profile
    if not trustMatches
        AstrolabePublish({ kind: "message", title: "Trust profile refused", body: "Coordinator does not match the bootstrap." })
        return
    end if

    nonce = AstrolabeRandomHex()
    pollKey = AstrolabeRandomHex()
    response = AstrolabeJson("/head/v1/pairings", "POST", {
        protocol_major: 1,
        receiver_nonce: nonce,
        poll_key: pollKey,
        rendezvous: m.rendezvous,
        receiver_id: AstrolabeReceiverId(m.coordinatorProfile),
        capabilities: AstrolabeCapabilities()
    })
    if response <> invalid and response.status = 403 and m.rendezvous <> invalid
        AstrolabePublish({ kind: "message", title: "Code not accepted", body: "A code works once. Get a new one in Astrolabe." })
        return
    end if
    if response = invalid or response.status <> 200 or response.json = invalid then return
    pairing = response.json
    if not AstrolabeExactFields(pairing, ["protocol_major", "pairing", "expires_in_ms", "confirmation_phrase", "coordinator_fingerprint", "coordinator_profile"]) then return
    if pairing.protocol_major <> 1 or not AstrolabeIntegerIn(pairing.expires_in_ms, 1, 600000) then return
    if not AstrolabeIsHex(pairing.pairing, 32) or not AstrolabeIsHex(pairing.coordinator_fingerprint, 64) then return
    if not AstrolabeIsProfileId(pairing.coordinator_profile) then return
    if m.fingerprint <> invalid and pairing.coordinator_fingerprint <> m.fingerprint then return
    if pairing.coordinator_profile <> m.coordinatorProfile then return
    phrase = AstrolabeConfirmationPhrase(pairing.coordinator_profile, pairing.pairing, nonce)
    if FormatJson(phrase) <> FormatJson(pairing.confirmation_phrase) then return
    m.credential = {
        mode: "pairing",
        origin: m.origin,
        pairing: pairing.pairing,
        receiverNonce: nonce,
        pollKey: pollKey,
        fingerprint: pairing.coordinator_fingerprint,
        profile: pairing.coordinator_profile,
        phrase: phrase,
        userConfirmed: m.rendezvous <> invalid ' a code typed in Astrolabe is the confirmation
    }
    if not AstrolabeSaveCredential(m.credential) then return
    AstrolabeContinuePairing()
end sub

sub AstrolabeContinuePairing()
    AstrolabePublish({
        kind: "pairing",
        phrase: m.credential.phrase,
        fingerprint: m.credential.fingerprint,
        confirmed: m.credential.userConfirmed
    })
    while m.credential.mode = "pairing" and not m.credential.userConfirmed
        command = m.top.command
        if Left(command, 8) = "confirm:"
            m.credential.userConfirmed = true
            if not AstrolabeSaveCredential(m.credential) then return
            exit while
        else if Left(command, 7) = "cancel:"
            AstrolabeClearCredential()
            m.credential = invalid
            return
        end if
        Sleep(100)
    end while
    if m.credential = invalid then return
    AstrolabePublish({ kind: "pairing", phrase: m.credential.phrase, fingerprint: m.credential.fingerprint, confirmed: true })

    while m.credential.mode = "pairing"
        status = AstrolabeJson("/head/v1/pairings/status", "POST", {
            protocol_major: 1,
            pairing: m.credential.pairing,
            proof: AstrolabePairingStatusTag(m.credential.pollKey, m.credential.pairing)
        })
        if m.credential = invalid then return
        if status = invalid or status.json = invalid or not status.json.DoesExist("kind")
            Sleep(3000)
        else if status.json.kind = "pending"
            if not AstrolabeExactFields(status.json, ["kind", "retry_after_ms"]) then return
            if not AstrolabeIntegerIn(status.json.retry_after_ms, 1, 60000) then return
            delayMs = status.json.retry_after_ms
            if delayMs < 1000 then delayMs = 1000
            if delayMs > 60000 then delayMs = 60000
            Sleep(delayMs)
        else if status.json.kind = "approved"
            if not AstrolabeExactFields(status.json, ["kind", "device", "proof_key", "enrollment_challenge"]) then return
            if not AstrolabeIsHex(status.json.device, 32) or not AstrolabeIsHex(status.json.proof_key, 64) or not AstrolabeIsHex(status.json.enrollment_challenge, 64) then return
            m.credential = {
                mode: "enrolling",
                origin: m.origin,
                pairing: m.credential.pairing,
                device: status.json.device,
                proofKey: status.json.proof_key,
                enrollmentChallenge: status.json.enrollment_challenge
            }
            if not AstrolabeSaveCredential(m.credential) then return
            AstrolabePublish({ kind: "message", title: "Enrolling…", body: "" })
            AstrolabeFinishEnrollment()
        else if status.json.kind = "rejected"
            if not AstrolabeExactFields(status.json, ["kind", "reason"]) then return
            if status.json.reason <> "user_rejected" and status.json.reason <> "controller_unavailable" and status.json.reason <> "policy_refused" and status.json.reason <> "fingerprint_mismatch" then return
            AstrolabePublish({ kind: "message", title: "Pairing stopped", body: "Rejected. Press Back, then relaunch." })
            return
        else if status.json.kind = "expired"
            if not AstrolabeExactFields(status.json, ["kind"]) then return
            AstrolabePublish({ kind: "message", title: "Pairing stopped", body: "Expired. Press Back, then relaunch." })
            return
        else
            return
        end if
    end while
end sub

sub AstrolabeFinishEnrollment()
    response = AstrolabeJson("/head/v1/pairings/complete", "POST", {
        protocol_major: 1,
        pairing: m.credential.pairing,
        device: m.credential.device,
        enrollment_challenge: m.credential.enrollmentChallenge,
        proof: AstrolabePairingCompleteTag(m.credential.proofKey, m.credential.pairing, m.credential.device, m.credential.enrollmentChallenge)
    })
    if response = invalid or response.status <> 200 or response.json = invalid then return
    if not AstrolabeExactFields(response.json, ["kind", "device", "next_challenge"]) then return
    if response.json.kind <> "enrolled" and response.json.kind <> "already_enrolled" then return
    if response.json.device <> m.credential.device or not AstrolabeIsHex(response.json.next_challenge, 64) then return
    m.challenge = response.json.next_challenge
    m.credential = { mode: "paired", origin: m.origin, device: m.credential.device, proofKey: m.credential.proofKey }
    AstrolabeSaveCredential(m.credential)
end sub

function AstrolabeStageProgram(program as object) as dynamic
    if not AstrolabeVerifyProgram(program) then return invalid
    stage = {}
    stagedBytes = 0
    for each item in program.items
        if item.scene.kind = "frame" and not stage.DoesExist(item.scene.asset.id)
            stagedBytes = stagedBytes + item.scene.asset.encoded_len
            if stagedBytes > 50331648 then return invalid
            entry = AstrolabeAuthorizedAsset(item.scene.asset, program)
            if entry = invalid then return invalid
            stage[item.scene.asset.id] = entry
        else if item.scene.kind = "media" and not stage.DoesExist(item.scene.manifest.id)
            ' A media manifest is streamed by the player segment by segment, not
            ' downloaded whole into memory, so its length is not staged bytes and
            ' does not count against the download cap. A whole-program stream is
            ' large by construction; capping it here is what refused it.
            entry = AstrolabeAuthorizedLive(item, program)
            if entry = invalid then return invalid
            stage[item.scene.manifest.id] = entry
        end if
    end for
    return stage
end function

sub AstrolabeRetireStage(stage as dynamic)
    if stage = invalid then return
    filesystem = CreateObject("roFileSystem")
    for each id in stage
        if Left(stage[id].uri, 5) = "tmp:/" then filesystem.Delete(stage[id].uri)
    end for
end sub

function AstrolabeAdoptCursor(playback as object, programDelivery = false as boolean) as boolean
    if m.program = invalid or not AstrolabeExactFields(playback, ["current_index", "elapsed_ms", "cycle", "sync"]) then return false
    if not AstrolabeValidSyncTarget(playback.sync) then return false
    if playback.current_index < 0 or playback.current_index >= m.program.items.Count() or playback.elapsed_ms < 0 then return false
    if playback.cycle <> m.program.playback.cycle then return false
    current = m.program.items[playback.current_index]
    if current.duration_ms <> invalid and playback.elapsed_ms >= current.duration_ms then return false
    previous = AstrolabeCurrentPlayback()
    if playback.sync <> invalid
        residual = 0
        if previous.currentIndex = playback.current_index then residual = playback.elapsed_ms - previous.elapsedMs
        if residual < -60000 then residual = -60000
        if residual > 60000 then residual = 60000
        m.lastSyncResidualMs = residual
        if previous.currentIndex <> playback.current_index or residual <> 0
            if m.correctionEvents < 2147483647 then m.correctionEvents = m.correctionEvents + 1
        end if
    else
        m.lastSyncResidualMs = 0
    end if
    m.program.playback.current_index = playback.current_index
    m.program.playback.elapsed_ms = playback.elapsed_ms
    m.program.playback.sync = playback.sync
    m.elapsedBase = playback.elapsed_ms
    m.playbackClock.Mark()
    if programDelivery then m.lastProgramDelivery.Mark()
    AstrolabeRenderCurrent()
    return true
end function

sub AstrolabeRenderCurrent()
    if m.program = invalid then return
    item = m.program.items[m.program.playback.current_index]
    source = m.program.program_state.kind
    if item.source_state.kind = "unavailable" or source = "unavailable"
        source = "unavailable"
    else if item.source_state.kind = "partial" or source = "partial"
        source = "partial"
    else
        source = "current"
    end if
    stale = m.lastProgramDelivery.TotalMilliseconds() >= m.program.freshness.stale_after_ms
    m.lastRenderedStale = stale
    if stale and m.program.freshness.on_stale = "blank"
        AstrolabePublish({ kind: "message", title: "Coordinator unavailable", body: "Assigned content expired.", source: source, stale: true })
        return
    end if
    if item.scene.kind = "frame"
        entry = m.stage[item.scene.asset.id]
        AstrolabePublish({
            kind: "frame",
            uri: entry.uri,
            expectedWidth: entry.width,
            expectedHeight: entry.height,
            spokenSummary: item.spoken_summary,
            source: source,
            stale: stale
        })
    else if item.scene.kind = "media"
        entry = m.stage[item.scene.manifest.id]
        ' The still that follows a clip is named now, so the scene can hold it
        ' decoded behind the player and cut to it before the player runs dry.
        nextFrame = invalid
        nextIndex = m.program.playback.current_index + 1
        if nextIndex >= m.program.items.Count() and m.program.playback.cycle = "loop" then nextIndex = 0
        if nextIndex < m.program.items.Count()
            nextItem = m.program.items[nextIndex]
            if nextItem.scene.kind = "frame" and m.stage.DoesExist(nextItem.scene.asset.id)
                nextEntry = m.stage[nextItem.scene.asset.id]
                nextFrame = { uri: nextEntry.uri, expectedWidth: nextEntry.width, expectedHeight: nextEntry.height }
            end if
        end if
        AstrolabePublish({
            kind: "media",
            uri: entry.uri,
            certificates: entry.certificates,
            nextFrame: nextFrame,
            loopInPlace: AstrolabeStreamsInPlace(),
            spokenSummary: item.spoken_summary,
            source: source,
            stale: stale
        })
    else
        AstrolabePublish({ kind: "blank", reason: item.scene.reason, source: source, stale: stale })
    end if
end sub

' Whether the program is one stream the player holds for as long as the
' assignment lasts: a single media item that either loops or has no end. The
' coordinator's whole-program stream is the second — an open-ended item under a
' program that holds its last, because the stream is endless and its length is
' not a fact the wire should carry. Either way the receiver never advances past
' it, never cuts it early, and never swaps the player's content.
function AstrolabeStreamsInPlace() as boolean
    if m.program = invalid or m.program.items.Count() <> 1 then return false
    item = m.program.items[0]
    if item.scene.kind <> "media" then return false
    return m.program.playback.cycle = "loop" or item.duration_ms = invalid
end function

sub AstrolabeTickPlayback()
    if m.program = invalid then return
    staleNow = m.lastProgramDelivery.TotalMilliseconds() >= m.program.freshness.stale_after_ms
    if m.lastRenderedStale = invalid or staleNow <> m.lastRenderedStale then AstrolabeRenderCurrent()
    item = m.program.items[m.program.playback.current_index]
    ' A program that is one stream is played by the player in place. Advancing
    ' the program here would swap the player's content and paint black at the
    ' seam, so the tick clears any spurious finish and lets the stream run.
    if AstrolabeStreamsInPlace()
        if Left(m.top.command, 15) = "media_finished:" then m.top.command = ""
        return
    end if
    ' A clip that reached its own end moves the program on at once; the
    ' slot's duration is a ceiling, not a wait.
    finished = item.scene.kind = "media" and Left(m.top.command, 15) = "media_finished:"
    if finished then m.top.command = ""
    if item.duration_ms = invalid and not finished then return
    if not finished and AstrolabeCurrentPlayback().elapsedMs < item.duration_ms then return
    nextIndex = m.program.playback.current_index + 1
    if nextIndex >= m.program.items.Count()
        if m.program.playback.cycle = "loop"
            nextIndex = 0
        else if m.program.playback.cycle = "blank_at_end"
            AstrolabePublish({ kind: "message", title: "Program complete", body: "" })
            return
        else if m.program.playback.cycle = "poll_at_end"
            AstrolabeRetireStage(m.stage)
            m.program = invalid
            m.stage = {}
            return
        else
            return
        end if
    end if
    m.program.playback.current_index = nextIndex
    m.program.playback.elapsed_ms = 0
    m.elapsedBase = 0
    m.playbackClock.Mark()
    AstrolabeRenderCurrent()
end sub

sub AstrolabeHandleProgramResponse(response as dynamic)
    if response = invalid or response.json = invalid or not response.json.DoesExist("kind") then return
    body = response.json
    if body.kind = "snapshot"
        if not AstrolabeExactFields(body, ["kind", "program"]) then return
        if not AstrolabeVerifyProgram(body.program) then return
        if m.program <> invalid and m.program.revision = body.program.revision
            m.program = body.program
            AstrolabeAdoptCursor(body.program.playback, true)
            return
        end if
        staged = AstrolabeStageProgram(body.program)
        if staged = invalid
            AstrolabePublish({ kind: "message", title: "Program refused", body: "Failed verification." })
            return
        end if
        previous = m.stage
        m.stage = staged
        m.program = body.program
        AstrolabeAdoptCursor(body.program.playback, true)
        AstrolabeRetireStage(previous)
    else if body.kind = "no_change"
        if not AstrolabeExactFields(body, ["kind", "revision", "playback"]) then return
        if m.program <> invalid and body.revision = m.program.revision
            AstrolabeAdoptCursor(body.playback, true)
        end if
    else if body.kind = "unassigned"
        if not AstrolabeExactFields(body, ["kind"]) then return
        AstrolabeRetireStage(m.stage)
        m.program = invalid
        m.stage = {}
        AstrolabePublish({ kind: "unassigned", device: m.credential.device })
        m.lastProgramDelivery.Mark()
    else if body.kind = "reset"
        if not AstrolabeExactFields(body, ["kind", "reason"]) then return
        AstrolabeRetireStage(m.stage)
        m.program = invalid
        m.stage = {}
    else if body.kind = "revoked"
        if not AstrolabeExactFields(body, ["kind"]) then return
        AstrolabeRetireStage(m.stage)
        m.program = invalid
        m.stage = {}
        m.challenge = invalid
        m.credential.mode = "revoked"
        AstrolabePublish({ kind: "message", title: "Screen revoked", body: "" })
    else if body.kind = "re_pair"
        if not AstrolabeExactFields(body, ["kind"]) then return
        AstrolabeRetireStage(m.stage)
        AstrolabeClearCredential()
        m.credential = invalid
        m.program = invalid
        m.stage = {}
        AstrolabePublish({ kind: "message", title: "Pairing required again", body: "Trust or identity changed." })
    end if
end sub

sub AstrolabeReportHealth()
    if m.program = invalid then return
    playback = AstrolabeCurrentPlayback()
    item = m.program.items[playback.currentIndex]
    displayed = invalid
    if item.scene.kind = "frame"
        displayed = { id: item.scene.asset.id, sha256: item.scene.asset.sha256 }
    end if
    stagedBytes = 0
    for each id in m.stage
        stagedBytes = stagedBytes + m.stage[id].bytes
    end for
    playbackState = "blank"
    if item.scene.kind = "frame" or item.scene.kind = "media" then playbackState = "displaying"
    ' The header and the body must say the same elapsed time; computing it
    ' twice put them milliseconds apart and every report was refused.
    response = AstrolabeAuthorizedJson("health", "POST", "/head/v1/health", {
        protocol_major: 1,
        platform: "roku",
        build: AstrolabeBuild(),
        revision: m.program.revision,
        current_item: item.id,
        elapsed_ms: playback.elapsedMs,
        last_displayed_asset: displayed,
        connection: "online",
        playback: playbackState,
        last_error: "none",
        staged_items: m.stage.Count(),
        staged_bytes: stagedBytes,
        decode_latency: "unobserved",
        swap_latency: "unobserved",
        drift_residual_ms: m.lastSyncResidualMs,
        correction_events: m.correctionEvents,
        pipeline_unobservable: true
    }, { currentItem: item.id, elapsedMs: playback.elapsedMs })
    if response <> invalid and response.status >= 200 and response.status < 300
        m.lastHealth.Mark()
    end if
end sub

sub AstrolabeProgramLoop()
    capabilityBackoff = 1000
    while m.credential <> invalid and m.credential.mode = "paired"
        capabilities = AstrolabeAuthorizedJson("capabilities", "POST", "/head/v1/capabilities", AstrolabeCapabilities())
        if capabilities <> invalid and capabilities.status >= 200 and capabilities.status < 300
            if capabilities.json = invalid then return
            if not AstrolabeExactFields(capabilities.json, ["kind"]) or capabilities.json.kind <> "accepted" then return
            exit while
        end if
        if capabilities <> invalid
            if AstrolabeValidApiError(capabilities.json)
                if capabilities.json.code = "revoked"
                    AstrolabeHandleProgramResponse({ json: { kind: "revoked" } })
                    return
                else if capabilities.json.code = "re_pair_required"
                    AstrolabeHandleProgramResponse({ json: { kind: "re_pair" } })
                    return
                end if
            end if
        end if
        m.transport = "offline"
        Sleep(capabilityBackoff)
        capabilityBackoff = capabilityBackoff * 2
        if capabilityBackoff > 30000 then capabilityBackoff = 30000
    end while
    while m.credential <> invalid and m.credential.mode = "paired"
        if Left(m.top.command, 13) = "media_failed:"
            if AstrolabeRefreshLive()
                continue while
            end if
        end if
        if m.program = invalid
            response = AstrolabeAuthorizedJson("program_snapshot", "GET", "/head/v1/program", invalid)
        else
            response = AstrolabeAuthorizedJson("program_changes", "GET", "/head/v1/program/changes", invalid, { waitMs: 25000 }, 35000)
        end if
        if response = invalid
            m.transport = "offline"
            AstrolabeTickPlayback()
            Sleep(1000)
        else
            if response.status < 200 or response.status >= 300
                if AstrolabeValidApiError(response.json)
                    if response.json.code = "revoked"
                        AstrolabeHandleProgramResponse({ json: { kind: "revoked" } })
                    else if response.json.code = "re_pair_required"
                        AstrolabeHandleProgramResponse({ json: { kind: "re_pair" } })
                    end if
                end if
            else
                AstrolabeHandleProgramResponse(response)
            end if
            if m.program <> invalid and m.lastHealth.TotalMilliseconds() >= 30000 then AstrolabeReportHealth()
            if m.program = invalid then Sleep(5000)
        end if
    end while
end sub

sub AstrolabeRun()
    ' The first thing the console says, so "which receiver is this" is a
    ' string to read, never a line number to trust.
    print "[astrolabe] build "; AstrolabeBuild()
    m.port = CreateObject("roMessagePort")
    m.transport = "connecting"
    bootstrap = AstrolabeReceiverBootstrap()
    if bootstrap = invalid
        AstrolabePublish({ kind: "message", title: "Setup refused", body: "Bootstrap is invalid." })
        return
    end if
    m.origin = bootstrap.origin
    m.rendezvous = bootstrap.rendezvous
    m.trustKind = bootstrap.trustKind
    m.fingerprint = bootstrap.fingerprint
    m.certificates = bootstrap.certificates
    m.challenge = invalid
    m.program = invalid
    m.stage = {}
    m.elapsedBase = 0
    m.lastSyncResidualMs = 0
    m.correctionEvents = 0
    m.lastRenderedStale = invalid
    m.playbackClock = CreateObject("roTimespan")
    m.lastProgramDelivery = CreateObject("roTimespan")
    m.lastHealth = CreateObject("roTimespan")
    m.playbackClock.Mark()
    m.lastProgramDelivery.Mark()
    m.lastHealth.Mark()
    AstrolabePublish({ kind: "booting" })
    if not AstrolabeConformanceCheck()
        AstrolabePublish({ kind: "message", title: "Self-check failed", body: "" })
        return
    end if
    m.credential = AstrolabeLoadCredential()
    if m.credential <> invalid and m.credential.origin <> m.origin
        AstrolabePublish({ kind: "message", title: "Coordinator changed", body: "Stored identity belongs to another origin." })
        return
    end if
    if m.credential = invalid
        AstrolabePair()
        if m.credential = invalid then AstrolabePair()
    end if
    if m.credential = invalid then return
    if m.credential.mode = "pairing"
        AstrolabeContinuePairing()
    else if m.credential.mode = "enrolling"
        AstrolabeFinishEnrollment()
    end if
    if m.credential.mode = "paired"
        ' Enrolled, program not yet asked for: nothing to state but the chip.
        AstrolabePublish({ kind: "message", title: "", body: "" })
        AstrolabeProgramLoop()
    end if
end sub
