sub init()
    m.media = m.top.FindNode("programMedia")
    ' Two stills, double-buffered. m.shown is the index on glass, or -1;
    ' m.ready is a verified still held hidden behind the player, or -1.
    m.posters = [m.top.FindNode("frameA"), m.top.FindNode("frameB")]
    m.posterW = [0, 0]
    m.posterH = [0, 0]
    m.shown = -1
    m.ready = -1
    m.retire = -1
    m.loadTarget = -1
    m.loadMode = "show"
    m.chrome = m.top.FindNode("chrome")
    m.brand = m.top.FindNode("brand")
    m.stage = m.top.FindNode("stage")
    m.title = m.top.FindNode("title")
    m.body = m.top.FindNode("body")
    m.phrase = m.top.FindNode("phraseGroup")
    m.readout = m.top.FindNode("readout")
    m.action = m.top.FindNode("action")
    m.details = m.top.FindNode("details")
    m.statusDot = m.top.FindNode("statusDot")
    m.statusLabel = m.top.FindNode("statusLabel")
    m.breathe = m.top.FindNode("breathe")
    m.task = m.top.FindNode("receiverTask")
    m.model = { kind: "booting" }
    m.sequence = 0
    m.mediaEnding = false
    m.frameThenYield = false
    ' One media item that loops is looped by the player in place, with no
    ' teardown between passes; the whole-program stream is exactly that.
    m.loopInPlace = false
    m.playingUri = invalid
    ' The last liveness state the chip announced, so a run of identical degraded
    ' polls does not keep re-lighting the HUD.
    m.statusState = ""
    ' Scene units per panel pixel: 1.5 on a 720p panel, 1 on a 1080p one.
    m.sceneRatio = 1.0
    display = CreateObject("roDeviceInfo").GetDisplaySize()
    if display <> invalid and display.w > 0 then m.sceneRatio = 1920.0 / display.w
    m.task.ObserveField("viewModel", "AstrolabeViewChanged")
    m.posters[0].ObserveField("loadStatus", "AstrolabeFrameALoaded")
    m.posters[1].ObserveField("loadStatus", "AstrolabeFrameBLoaded")
    m.media.ObserveField("state", "AstrolabeMediaStateChanged")
    m.media.notificationInterval = 0.25
    m.media.ObserveField("position", "AstrolabeMediaPositionChanged")
    m.mediaStop = m.top.FindNode("mediaStop")
    m.mediaStop.ObserveField("fire", "AstrolabeMediaStopDue")
    m.posterRetire = m.top.FindNode("posterRetire")
    m.posterRetire.ObserveField("fire", "AstrolabePosterRetireDue")
    m.statusHud = m.top.FindNode("statusHud")
    m.statusHud.ObserveField("fire", "AstrolabeStatusHudDue")
    ' The decoder's counters are only kept when asked for.
    m.media.enableDecoderStats = true
    m.pipelineSample = m.top.FindNode("pipelineSample")
    m.pipelineSample.ObserveField("fire", "AstrolabePipelineSample")
    m.pipelineSample.control = "start"
    m.top.FindNode("statusChip").visible = false
    m.keepAwake = m.top.FindNode("keepAwake")
    silence = CreateObject("roSGNode", "ContentNode")
    silence.url = "pkg:/media/silence.wav"
    m.keepAwake.content = silence
    AstrolabeStayAwake(true)
    m.top.SetFocus(true)
    m.task.control = "RUN"
end sub

' Roku allows one player at a time: the silent loop holds the screensaver off
' while a still or a message is on glass, and steps aside for real media,
' which holds it off by itself.
sub AstrolabeStayAwake(awake as boolean)
    if awake
        if m.keepAwake.state <> "playing" then m.keepAwake.control = "play"
    else
        m.keepAwake.control = "stop"
    end if
end sub

' The console's palette, flattened onto the panel.
function AstrolabeTone(name as string) as string
    if name = "positive" then return "0x45D69EFF"
    if name = "miss" then return "0xB8862BFF"
    if name = "alarm" then return "0xD8472EFF"
    if name = "muted" then return "0xA2A5A8FF"
    if name = "ink" then return "0xF2F3F4FF"
    return "0x787B7EFF"
end function

' The console's words for a screen: connecting is uncoloured, connected is
' the green of now, a failed transfer the loop keeps retrying is the ochre
' of a miss.
function AstrolabeTransportLabel(transport as string) as string
    if transport = "online" then return "Connected"
    if transport = "offline" then return "Reconnecting…"
    return "Connecting…"
end function

' The World's blank reasons, in the console's words.
function AstrolabeBlankTitle(reason as string) as string
    if reason = "unassigned" then return "Nothing reaches this screen"
    if reason = "host_unavailable" then return "Coordinator unavailable"
    if reason = "source_unavailable" then return "Source unavailable"
    if reason = "unsupported" then return "Program unsupported"
    if reason = "revoked" then return "Screen revoked"
    if reason = "program_ended" then return "Program complete"
    return AstrolabeSentence(reason.Replace("_", " "))
end function

' The liveness chip is a HUD, not a fixture. A healthy screen says nothing; a
' change of state names itself once, then the chip retires so nothing burns
' into the glass. The full chrome still carries a state that has no program to
' play behind it, so retiring the chip never hides an unrecoverable screen.
sub AstrolabeStatus(transport as string, stale as boolean)
    chip = m.top.FindNode("statusChip")
    if transport = "online" and not stale
        m.breathe.control = "stop"
        m.statusHud.control = "stop"
        chip.visible = false
        m.statusState = "online-fresh"
        return
    end if
    ' Only reveal — and restart the retire countdown — when the state actually
    ' changes, so a run of degraded polls does not keep the chip lit.
    state = transport
    if stale then state = "stale"
    reveal = (m.statusState <> state)
    m.statusState = state
    if stale
        m.statusLabel.text = "Stale"
    else
        m.statusLabel.text = AstrolabeTransportLabel(transport)
    end if
    pill = m.top.FindNode("statusPill")
    pill.width = 64 + m.statusLabel.localBoundingRect().width + 32
    chip.translation = [1824 - pill.width, 44]
    m.breathe.control = "stop"
    m.statusDot.opacity = 1.0
    if transport = "offline" or stale
        m.statusDot.blendColor = AstrolabeTone("miss")
    else
        m.statusDot.blendColor = AstrolabeTone("faint")
    end if
    if reveal
        chip.visible = true
        m.statusHud.control = "stop"
        m.statusHud.control = "start"
    end if
end sub

' The chip has had its moment. Retire it; a further change re-reveals it, and a
' screen with nothing to play keeps its chrome regardless.
' Every few seconds, what the player is actually doing: its state and
' position, the decoder's cumulative render/drop/repeat/error counts, the
' segment it is on, and whether it is buffering. Handed to the task for the
' health report and said on the console, so a stutter is a number here and
' at the coordinator, never only a person's impression of the picture.
sub AstrolabePipelineSample()
    if m.media = invalid then return
    stats = m.media.decoderStats
    if stats = invalid then stats = {}
    segment = m.media.streamingSegment
    sequence = invalid
    if segment <> invalid and segment.segSequence <> invalid then sequence = segment.segSequence
    buffering = m.media.bufferingStatus
    underflow = false
    if buffering <> invalid and buffering.isUnderflow = true then underflow = true
    state = m.media.state
    if state = invalid or state = "" then state = "none"
    sample = {
        state: state,
        position_ms: Int(m.media.position * 1000),
        frames_rendered: AstrolabeCount(stats.renderCount),
        frames_dropped: AstrolabeCount(stats.frameDropCount),
        frames_repeated: AstrolabeCount(stats.repeatCount),
        stream_errors: AstrolabeCount(stats.streamErrorCount),
        segment_sequence: sequence,
        buffering: underflow or state = "buffering"
    }
    m.task.pipeline = sample
    print "[astrolabe] pipeline state="; sample.state; " pos_ms="; sample.position_ms; " rendered="; sample.frames_rendered; " dropped="; sample.frames_dropped; " repeated="; sample.frames_repeated; " errors="; sample.stream_errors; " seq="; sequence; " buffering="; sample.buffering
end sub

function AstrolabeCount(value as dynamic) as integer
    if value = invalid then return 0
    if value < 0 then return 0
    if value > 2147483647 then return 2147483647
    return Int(value)
end function

sub AstrolabeStatusHudDue()
    m.top.FindNode("statusChip").visible = false
end sub

function AstrolabeSentence(word as string) as string
    if word = "" then return ""
    return UCase(Left(word, 1)) + Mid(word, 2)
end function

' Load a still into whichever poster is not on glass, then either show it as
' soon as it verifies ("show") or hold it verified and hidden behind the
' player until a cut reveals it ("hold"). The poster on glass is never
' touched, so nothing is cleared before its replacement exists.
sub AstrolabeLoadFrame(uri as string, width as integer, height as integer, mode as string)
    back = 0
    if m.shown = 0 then back = 1
    m.loadTarget = back
    m.loadMode = mode
    m.posterW[back] = width
    m.posterH[back] = height
    poster = m.posters[back]
    if poster.uri = uri and poster.loadStatus = "ready"
        AstrolabeFrameVerified(back)
    else
        poster.visible = false
        poster.uri = uri
    end if
end sub

' Bring a verified still to glass. The one it replaces is left up and retired
' a frame later, so the swap never leaves a tick with neither poster painted
' and the background or the video plane showing through.
sub AstrolabeRevealFrame(index as integer)
    m.posters[index].visible = true
    previous = m.shown
    m.shown = index
    m.ready = -1
    if previous >= 0 and previous <> index
        m.retire = previous
        m.posterRetire.control = "start"
    end if
end sub

sub AstrolabePosterRetireDue()
    if m.retire >= 0 and m.retire <> m.shown then m.posters[m.retire].visible = false
    m.retire = -1
end sub

sub AstrolabeHideFrames()
    m.posters[0].visible = false
    m.posters[1].visible = false
    m.shown = -1
    m.ready = -1
    m.retire = -1
end sub

' Chrome and stage return for any fact worth stating; the program layers go dark.
sub AstrolabeShowStage(raised as boolean)
    m.brand.visible = true
    m.stage.visible = true
    AstrolabeHideFrames()
    m.media.control = "stop"
    m.media.visible = false
    AstrolabeStayAwake(true)
    if raised
        m.stage.translation = [0, 0]
    else
        m.stage.translation = [0, 230]
    end if
end sub

' The chrome retires once a program is on the glass and nothing is wrong;
' it returns for a transport that is not connected or a delivery gone stale.
sub AstrolabeRetireChrome(model as object)
    ' A program is on glass: the brand steps off it entirely. A transport or
    ' freshness problem is carried by the liveness HUD, which is its own layer
    ' now, so it can flash over the picture without the brand returning.
    m.brand.visible = false
    m.stage.visible = false
    m.details.visible = false
end sub

' A refusal or a revocation is alarm; an absence the loop will outgrow is a
' miss; everything else is plain ink.
function AstrolabeMessageTone(title as string) as string
    lowered = LCase(title)
    if lowered.Instr("refused") >= 0 or lowered.Instr("revoked") >= 0 or lowered.Instr("failed") >= 0 then return "alarm"
    if lowered.Instr("unavailable") >= 0 or lowered.Instr("stopped") >= 0 or lowered.Instr("again") >= 0 then return "miss"
    return "ink"
end function

sub AstrolabeMessage(title as string, body as string)
    AstrolabeShowStage(false)
    m.phrase.visible = false
    m.readout.visible = false
    m.action.visible = false
    m.title.text = title
    m.title.color = AstrolabeTone(AstrolabeMessageTone(title))
    m.body.text = body
end sub

sub AstrolabeReadout(label as string, value as string, y as integer)
    m.readout.visible = true
    m.readout.translation = [160, y]
    m.top.FindNode("readoutLabel").text = label
    m.top.FindNode("readoutValue").text = value
end sub

' A fingerprint is compared by eye, so it is grouped the way the console groups it.
function AstrolabeGrouped(hex as string) as string
    grouped = ""
    position = 1
    while position <= Len(hex)
        if position = 33
            grouped = grouped + Chr(10)
        else if grouped <> ""
            grouped = grouped + "  "
        end if
        grouped = grouped + Mid(hex, position, 8)
        position = position + 8
    end while
    return grouped
end function

sub AstrolabeViewChanged()
    model = m.task.viewModel
    if model = invalid then return
    m.model = model
    AstrolabeStatus(model.transport, model.stale)
    m.top.FindNode("detailSource").text = AstrolabeSentence(model.source)
    if model.stale
        m.top.FindNode("detailDelivery").text = "Stale"
    else
        m.top.FindNode("detailDelivery").text = "Current"
    end if

    if model.kind = "booting"
        AstrolabeMessage("Starting…", "")
    else if model.kind = "pairing"
        AstrolabeShowStage(true)
        m.title.text = "Compare with Astrolabe"
        m.title.color = AstrolabeTone("ink")
        m.body.text = ""
        m.phrase.visible = true
        for index = 0 to 5
            m.top.FindNode("phrase" + index.ToStr()).text = UCase(model.phrase[index])
        end for
        AstrolabeReadout("Coordinator SHA-256", AstrolabeGrouped(model.fingerprint), 700)
        m.action.visible = true
        pill = m.top.FindNode("actionPill")
        label = m.top.FindNode("actionLabel")
        if model.confirmed
            pill.blendColor = "0x1D2126FF"
            m.top.FindNode("actionKey").visible = false
            label.color = AstrolabeTone("muted")
            label.text = "Waiting for Astrolabe…"
            label.translation = [32, 0]
            pill.width = 32 + label.localBoundingRect().width + 32
        else
            pill.blendColor = "0x3B96F2FF"
            m.top.FindNode("actionKey").visible = true
            label.color = "0x06121FFF"
            label.text = "Same words"
            label.translation = [116, 0]
            pill.width = 116 + label.localBoundingRect().width + 32
        end if
        m.action.translation = [(1920 - pill.width) / 2, 900]
    else if model.kind = "unassigned"
        AstrolabeMessage("Nothing reaches this screen", "")
        AstrolabeReadout("Screen", model.device, 310)
    else if model.kind = "blank"
        AstrolabeMessage(AstrolabeBlankTitle(model.reason), "")
    else if model.kind = "frame"
        AstrolabeRetireChrome(model)
        AstrolabeStayAwake(true)
        mediaActive = m.media.visible
        if m.shown >= 0 and m.posters[m.shown].uri = model.uri
            ' Already on glass — the early cut at the clip's tail beat the
            ' program advance here. Nothing to swap; just retire the player.
            if mediaActive then AstrolabeYieldMediaToFrame()
        else if m.ready >= 0 and m.posters[m.ready].uri = model.uri
            ' Already verified behind the player — this is the cut out of a
            ' clip onto its next still. Reveal it, then let the player go.
            AstrolabeRevealFrame(m.ready)
            if mediaActive then AstrolabeYieldMediaToFrame()
        else
            ' A still after a still, or the first still: load into the spare
            ' poster and swap when it verifies. The current one stays up. If a
            ' clip is still on glass, stop it once the still has replaced it.
            m.frameThenYield = mediaActive
            AstrolabeLoadFrame(model.uri, model.expectedWidth, model.expectedHeight, "show")
        end if
    else if model.kind = "media"
        AstrolabeRetireChrome(model)
        m.mediaEnding = false
        m.frameThenYield = false
        AstrolabeStayAwake(false)
        ' The still on glass keeps covering while the player buffers; it is
        ' hidden only once a real picture is playing. The next still is
        ' decoded behind the player so the cut out of the clip has somewhere
        ' to land.
        if model.nextFrame <> invalid
            AstrolabeLoadFrame(model.nextFrame.uri, model.nextFrame.expectedWidth, model.nextFrame.expectedHeight, "hold")
        end if
        ' The view model is republished on every poll answer, and a whole-
        ' program stream keeps one URL for the life of the assignment. Handing
        ' the player the content it is already playing would reload it — a
        ' rebuffer every 25 s on a stream that never changed — so an unchanged
        ' URL on a playing player is left alone.
        if m.playingUri = model.uri and m.media.visible and (m.media.state = "playing" or m.media.state = "buffering")
            m.loopInPlace = (model.loopInPlace = true)
            m.media.loop = m.loopInPlace
            return
        end if
        content = CreateObject("roSGNode", "ContentNode")
        content.url = model.uri
        content.streamFormat = "hls"
        content.title = model.spokenSummary
        ' The coordinator's certificate is pinned for API calls; the player
        ' verifies its own connection, so it is handed the same file.
        if model.certificates <> invalid then content.HttpCertificatesFile = model.certificates
        ' A whole-program stream loops in the player itself, so the seam back to
        ' the first slide is a decoder rewind rather than a fresh content load —
        ' no teardown, no re-buffer, no black at the wrap.
        m.loopInPlace = (model.loopInPlace = true)
        m.media.loop = m.loopInPlace
        m.playingUri = model.uri
        m.media.content = content
        m.media.visible = true
        m.media.control = "play"
    else
        AstrolabeMessage(model.title, model.body)
    end if
end sub

' Hide the player behind the still that now covers it, and stop it a beat
' later, so the stop's own black flash lands behind the still and not on the
' cut. Also wakes the silent loop, since a still is on glass again.
sub AstrolabeYieldMediaToFrame()
    m.media.visible = false
    m.mediaStop.control = "start"
    AstrolabeStayAwake(true)
end sub

' The player's clock says when the clip is about to run dry. If the next
' still is already verified behind it, reveal it now so the cut lands on a
' picture rather than the black the player paints as it drains, then move the
' program on.
sub AstrolabeMediaPositionChanged()
    if m.loopInPlace then return
    if m.mediaEnding or m.media.state <> "playing" then return
    if m.media.duration <= 0 or m.media.position < m.media.duration - 0.6 then return
    m.mediaEnding = true
    if m.ready >= 0
        AstrolabeRevealFrame(m.ready)
        AstrolabeYieldMediaToFrame()
    end if
    m.sequence = m.sequence + 1
    m.task.command = "media_finished:" + m.sequence.ToStr()
end sub

sub AstrolabeMediaStopDue()
    if m.model.kind = "media" then return ' a new clip took the player back
    m.media.control = "stop"
    m.playingUri = invalid
end sub

sub AstrolabeMediaStateChanged()
    if m.media.state = "playing"
        ' A real picture is up; the still that covered the buffering can go.
        ' The preloaded next still (m.ready) is a different poster and is left
        ' hidden and intact for the cut out of the clip.
        if m.shown >= 0 and m.shown <> m.ready
            m.posters[m.shown].visible = false
            m.shown = -1
        end if
    else if m.media.state = "finished" and not m.mediaEnding and not m.loopInPlace
        ' The clip ran dry before its clock crossed the early-cut line. If the
        ' next still is verified, reveal it now so the player's end-of-stream
        ' black lands behind a picture; then move on.
        m.mediaEnding = true
        if m.ready >= 0
            AstrolabeRevealFrame(m.ready)
            AstrolabeYieldMediaToFrame()
        end if
        m.sequence = m.sequence + 1
        m.task.command = "media_finished:" + m.sequence.ToStr()
    end if
    if m.media.state = "error"
        AstrolabeMessage("Live media decode failed", "")
        m.sequence = m.sequence + 1
        m.task.command = "media_failed:" + m.sequence.ToStr()
    end if
end sub

sub AstrolabeFrameALoaded()
    AstrolabeFrameStatus(0)
end sub

sub AstrolabeFrameBLoaded()
    AstrolabeFrameStatus(1)
end sub

sub AstrolabeFrameStatus(index as integer)
    if index <> m.loadTarget then return
    AstrolabeFrameVerified(index)
end sub

sub AstrolabeFrameVerified(index as integer)
    poster = m.posters[index]
    if poster.loadStatus = "ready"
        ' The scene is authored at 1920x1080 and SceneGraph reports a bitmap in
        ' scene units, so a frame rendered for a 1280x720 panel reads as
        ' 1920x1080 here. The authenticated dimensions are compared in the same
        ' units: pixels times the scene's ratio to the panel.
        expectedWidth = Int(m.posterW[index] * m.sceneRatio + 0.5)
        expectedHeight = Int(m.posterH[index] * m.sceneRatio + 0.5)
        if poster.bitmapWidth = expectedWidth and poster.bitmapHeight = expectedHeight
            ' Fit the verified frame to the panel, centred, without touching the bitmap.
            fit = 1920.0 / expectedWidth
            if 1080.0 / expectedHeight < fit then fit = 1080.0 / expectedHeight
            poster.scale = [fit, fit]
            poster.translation = [(1920 - expectedWidth * fit) / 2, (1080 - expectedHeight * fit) / 2]
            if m.loadMode = "hold"
                ' Verified and waiting behind the player for the cut.
                m.ready = index
            else
                AstrolabeRevealFrame(index)
                if m.frameThenYield
                    ' This still replaced a clip that was still on glass.
                    m.frameThenYield = false
                    AstrolabeYieldMediaToFrame()
                end if
            end if
        else
            poster.uri = ""
            AstrolabeMessage("Frame dimensions changed", "")
            m.sequence = m.sequence + 1
            m.task.command = "asset_failed:" + m.sequence.ToStr()
        end if
    else if poster.loadStatus = "failed"
        AstrolabeMessage("Frame decode failed", "")
    end if
end sub

function onKeyEvent(key as string, press as boolean) as boolean
    if not press then return false
    m.sequence = m.sequence + 1
    if key = "OK" and m.model.kind = "pairing" and not m.model.confirmed
        m.task.command = "confirm:" + m.sequence.ToStr()
        return true
    else if key = "info" or key = "options"
        m.details.visible = not m.details.visible
        return true
    else if key = "back" and m.model.kind = "pairing"
        m.task.command = "cancel:" + m.sequence.ToStr()
        return true
    end if
    return false
end function
