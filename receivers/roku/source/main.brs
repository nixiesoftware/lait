sub Main()
    screen = CreateObject("roSGScreen")
    port = CreateObject("roMessagePort")
    screen.SetMessagePort(port)
    scene = screen.CreateScene("AstrolabeScene")
    screen.Show()
    while true
        event = Wait(0, port)
        if Type(event) = "roSGScreenEvent" and event.IsScreenClosed() then return
    end while
end sub
