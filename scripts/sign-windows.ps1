<#
.SYNOPSIS
  Authenticode signing helpers for Wren's Windows release (CI and local checks).

.DESCRIPTION
  -Import     Imports a base64 PFX from $env:WINDOWS_CERTIFICATE (password $env:WINDOWS_CERTIFICATE_PASSWORD)
              into CurrentUser\My and prints its thumbprint (also to $env:GITHUB_OUTPUT as `thumbprint=`).
              Never prints the certificate or the password.
  -Config     Writes a Tauri config overlay that makes `tauri build` sign wren.exe, the installer and the
              uninstaller with that certificate (signtool, SHA-256, RFC 3161 timestamp). Pass it to
              `npx tauri build --config <file>`.
  -Verify     Checks every -Files entry carries an Authenticode signature by -Thumbprint. With
              -RequireTrusted the signature must also chain to a trusted root (Status = Valid); a real
              publisher certificate passes, a self-signed test certificate does not. Exit 1 on any miss.

.EXAMPLE
  # Local proof with a throwaway self-signed certificate (no publisher credentials needed):
  $c = New-SelfSignedCertificate -Type CodeSigningCert -Subject 'CN=Wren Test Signing' -CertStoreLocation Cert:\CurrentUser\My
  powershell -File scripts\sign-windows.ps1 -Config -Thumbprint $c.Thumbprint -Out $env:TEMP\wren-sign.json
  npx tauri build --bundles nsis --config $env:TEMP\wren-sign.json
  7z x -o"$env:TEMP\wren-payload" <installer> wren.exe uninstall.exe   # check what ships, not target\release\wren.exe
  powershell -File scripts\sign-windows.ps1 -Verify -Thumbprint $c.Thumbprint -Files <installer>,$env:TEMP\wren-payload\wren.exe,$env:TEMP\wren-payload\uninstall.exe
  Remove-Item Cert:\CurrentUser\My\$($c.Thumbprint)                       # throw the test certificate away
#>
param(
    [switch]$Import,
    [switch]$Config,
    [switch]$Verify,
    [string]$Thumbprint = '',
    [string]$TimestampUrl = 'http://timestamp.digicert.com',
    [string]$Out = '',
    [string[]]$Files = @(),
    [switch]$RequireTrusted
)
$ErrorActionPreference = 'Stop'

if ($Import) {
    if (-not $env:WINDOWS_CERTIFICATE -or -not $env:WINDOWS_CERTIFICATE_PASSWORD) {
        throw 'WINDOWS_CERTIFICATE and WINDOWS_CERTIFICATE_PASSWORD must both be set to sign'
    }
    $pfx = Join-Path ([IO.Path]::GetTempPath()) ("wren-sign-" + [guid]::NewGuid().ToString('N') + '.pfx')
    try {
        [IO.File]::WriteAllBytes($pfx, [Convert]::FromBase64String($env:WINDOWS_CERTIFICATE))
        $secure = ConvertTo-SecureString $env:WINDOWS_CERTIFICATE_PASSWORD -AsPlainText -Force
        $cert = Import-PfxCertificate -FilePath $pfx -CertStoreLocation Cert:\CurrentUser\My -Password $secure
    } finally {
        Remove-Item $pfx -Force -ErrorAction SilentlyContinue
    }
    $cert = @($cert | Where-Object { $_.HasPrivateKey })[0]
    if (-not $cert) { throw 'the PFX has no certificate with a private key' }
    if (-not ($cert.EnhancedKeyUsageList | Where-Object { $_.ObjectId -eq '1.3.6.1.5.5.7.3.3' })) {
        throw 'the certificate is not a code-signing certificate'
    }
    Write-Host "imported signing certificate $($cert.Thumbprint) ($($cert.Subject)), valid until $($cert.NotAfter.ToString('yyyy-MM-dd'))"
    if ($env:GITHUB_OUTPUT) { "thumbprint=$($cert.Thumbprint)" | Out-File -Append -Encoding utf8 $env:GITHUB_OUTPUT }
    exit 0
}

if ($Config) {
    if (-not $Thumbprint) { throw '-Thumbprint is required' }
    if (-not $Out) { throw '-Out is required' }
    $overlay = @{ bundle = @{ windows = @{ certificateThumbprint = $Thumbprint; digestAlgorithm = 'sha256'; timestampUrl = $TimestampUrl } } }
    $overlay | ConvertTo-Json -Depth 5 | Set-Content -Path $Out -Encoding ascii
    Write-Host "signing config written to $Out"
    exit 0
}

if ($Verify) {
    if (-not $Thumbprint) { throw '-Thumbprint is required' }
    if ($Files.Count -eq 0) { throw '-Files is required' }
    $bad = 0
    foreach ($f in $Files) {
        if (-not (Test-Path -LiteralPath $f)) { Write-Host "MISSING  $f"; $bad++; continue }
        $sig = Get-AuthenticodeSignature -LiteralPath $f
        $signer = if ($sig.SignerCertificate) { $sig.SignerCertificate.Thumbprint } else { '' }
        $stamped = [bool]$sig.TimeStamperCertificate
        $ok = $signer -eq $Thumbprint -and ($sig.Status -eq 'Valid' -or (-not $RequireTrusted -and $sig.Status -in @('Valid', 'UnknownError', 'NotTrusted')))
        Write-Host ("{0}  {1}  status={2} signer={3} timestamped={4}" -f $(if ($ok) { 'SIGNED ' } else { 'UNSIGNED' }), (Split-Path $f -Leaf), $sig.Status, $signer, $stamped)
        if (-not $ok) { $bad++ }
    }
    if ($bad -gt 0) { Write-Host "SIGNING CHECK FAIL: $bad file(s)"; exit 1 }
    Write-Host 'SIGNING CHECK OK'
    exit 0
}

throw 'use -Import, -Config or -Verify'
