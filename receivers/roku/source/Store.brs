function AstrolabeLoadCredential() as dynamic
    section = CreateObject("roRegistrySection", "AstrolabeReceiver")
    if not section.Exists("credential_v1") then return invalid
    encrypted = CreateObject("roByteArray")
    encrypted.FromBase64String(section.Read("credential_v1"))
    plaintext = CreateObject("roDeviceCrypto").Decrypt(encrypted, "device")
    if plaintext = invalid then return invalid
    return ParseJson(plaintext.ToAsciiString())
end function

function AstrolabeSaveCredential(credential as object) as boolean
    plaintext = AstrolabeByteArray(FormatJson(credential))
    if plaintext.Count() > 16384 then return false
    encrypted = CreateObject("roDeviceCrypto").Encrypt(plaintext, "device")
    if encrypted = invalid then return false
    section = CreateObject("roRegistrySection", "AstrolabeReceiver")
    if not section.Write("credential_v1", encrypted.ToBase64String()) then return false
    return section.Flush()
end function

function AstrolabeClearCredential() as boolean
    section = CreateObject("roRegistrySection", "AstrolabeReceiver")
    if section.Exists("credential_v1") and not section.Delete("credential_v1") then return false
    return section.Flush()
end function
