sub init()
    m.frame = m.top.FindNode("programFrame")
    m.panel = m.top.FindNode("statePanel")
    m.phrase = m.top.FindNode("phraseGroup")
    m.details = m.top.FindNode("details")
    m.task = m.top.FindNode("receiverTask")
    m.model = { kind: "booting" }
    m.sequence = 0
    m.expectedWidth = 0
    m.expectedHeight = 0
    m.task.ObserveField("viewModel", "AstrolabeViewChanged")
    m.frame.ObserveField("loadStatus", "AstrolabeFrameLoaded")
    m.top.SetFocus(true)
    m.task.control = "RUN"
end sub

sub AstrolabeMessage(eyebrow as string, title as string, body as string)
    m.panel.visible = true
    m.frame.visible = false
    m.phrase.visible = false
    m.top.FindNode("eyebrow").text = eyebrow
    m.top.FindNode("title").text = title
    m.top.FindNode("body").text = body
    m.top.FindNode("fingerprint").text = ""
    m.top.FindNode("action").text = ""
end sub

sub AstrolabeViewChanged()
    model = m.task.viewModel
    if model = invalid then return
    m.model = model
    m.top.FindNode("transportState").text = UCase(Left(model.transport, 1)) + Mid(model.transport, 2)
    m.top.FindNode("sourceState").text = UCase(Left(model.source, 1)) + Mid(model.source, 2)
    staleText = "Current"
    if model.stale then staleText = "Stale"
    m.top.FindNode("detailsText").text = "Transport  " + model.transport + Chr(10) + "Source  " + model.source + Chr(10) + "Delivery  " + staleText + Chr(10) + "Protocol  Astrolabe Display 1"

    if model.kind = "booting"
        AstrolabeMessage("ASTROLABE DISPLAY", "Starting this receiver…", "Opening protected device state and contacting the approved coordinator.")
    else if model.kind = "pairing"
        m.panel.visible = true
        m.frame.visible = false
        m.phrase.visible = true
        m.top.FindNode("eyebrow").text = "CONFIRM THIS DISPLAY"
        m.top.FindNode("title").text = "Compare these words in Astrolabe"
        m.top.FindNode("body").text = "Approve only if the words and full coordinator fingerprint match."
        for index = 0 to 5
            m.top.FindNode("phrase" + index.ToStr()).text = UCase(model.phrase[index])
        end for
        m.top.FindNode("fingerprint").text = model.fingerprint
        if model.confirmed
            m.top.FindNode("action").text = "Confirmed here — waiting for Astrolabe approval"
        else
            m.top.FindNode("action").text = "Press OK only when the words match"
        end if
    else if model.kind = "unassigned"
        AstrolabeMessage("RECEIVER ENROLLED", "Ready for an assignment", "Choose this display in Astrolabe Displays." + Chr(10) + model.device)
    else if model.kind = "frame"
        m.panel.visible = false
        m.frame.visible = false
        m.expectedWidth = model.expectedWidth
        m.expectedHeight = model.expectedHeight
        m.frame.audioGuideText = model.spokenSummary
        m.frame.uri = model.uri
    else
        AstrolabeMessage("RECEIVER-OWNED STATE", model.title, model.body)
    end if
end sub

sub AstrolabeFrameLoaded()
    if m.frame.loadStatus = "ready"
        if m.frame.bitmapWidth = m.expectedWidth and m.frame.bitmapHeight = m.expectedHeight
            m.frame.visible = true
        else
            m.frame.uri = ""
            AstrolabeMessage("RECEIVER REFUSED", "Frame dimensions changed", "The decoded frame did not match the authenticated program metadata.")
            m.sequence = m.sequence + 1
            m.task.command = "asset_failed:" + m.sequence.ToStr()
        end if
    else if m.frame.loadStatus = "failed"
        AstrolabeMessage("RECEIVER REFUSED", "Frame decode failed", "The verified bytes could not be decoded by this Roku device.")
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
